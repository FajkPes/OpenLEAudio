/*
 * HCI packet inspection and rewriting.
 *
 * Freestanding C: no CRT, no allocation, no OS calls. The same objects link into
 * the KMDF filter and into the user-mode test harness, so everything here must
 * stay safe to call at DISPATCH_LEVEL.
 *
 * Every parser validates lengths before reading. Every rewriter refuses to touch
 * a packet it cannot fully account for. A caller that ignores the return value
 * still ends up forwarding an unmodified packet - that is the intended failure mode.
 */

#ifndef OLEA_HCI_H
#define OLEA_HCI_H

#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>

/* ---- Opcodes we care about (OGF 0x08 = LE Controller) ---- */

#define HCI_OPCODE(ogf, ocf)            ((uint16_t)(((ogf) << 10) | (ocf)))
#define HCI_OP_LE_CONNECTION_UPDATE     HCI_OPCODE(0x08, 0x0013)
#define HCI_OP_LE_SET_PHY               HCI_OPCODE(0x08, 0x0032)
#define HCI_OP_LE_SET_CIG_PARAMETERS    HCI_OPCODE(0x08, 0x0062)
#define HCI_OP_LE_CREATE_CIS            HCI_OPCODE(0x08, 0x0064)

/* ---- Events ---- */

#define HCI_EVT_DISCONNECTION_COMPLETE  0x05
#define HCI_EVT_LE_META                 0x3E

#define HCI_SUBEVT_LE_ENHANCED_CONN_COMPLETE 0x0A
#define HCI_SUBEVT_LE_CIS_ESTABLISHED        0x19

/* ---- Limits ---- */

#define HCI_MAX_CIS_PER_CIG   0x1F
#define HCI_CIG_HEADER_LEN    15u  /* CIG_ID .. CIS_Count inclusive */
#define HCI_CIG_CIS_ENTRY_LEN 9u   /* one Set CIG Parameters CIS block */

/* PHY bitfield values used by LE Set CIG Parameters. */
#define HCI_PHY_1M    0x01
#define HCI_PHY_2M    0x02
#define HCI_PHY_CODED 0x04

/* ---- Views over a raw packet ---- */

/*
 * A parsed HCI command header. `params` points into the caller's buffer; the
 * struct never owns memory.
 */
typedef struct {
    uint16_t opcode;
    uint8_t  param_len;
    uint8_t *params;     /* mutable: rewriters write through this */
} hci_command;

typedef struct {
    uint8_t  event_code;
    uint8_t  subevent;   /* valid only when event_code == HCI_EVT_LE_META */
    uint8_t  param_len;
    const uint8_t *params;
} hci_event;

/* One CIS entry inside LE Set CIG Parameters. */
typedef struct {
    uint8_t  cis_id;
    uint16_t max_sdu_c_to_p;
    uint16_t max_sdu_p_to_c;
    uint8_t  phy_c_to_p;
    uint8_t  phy_p_to_c;
    uint8_t  rtn_c_to_p;
    uint8_t  rtn_p_to_c;
} hci_cis_params;

typedef struct {
    uint8_t  cig_id;
    uint32_t sdu_interval_c_to_p;   /* microseconds, 24-bit on the wire */
    uint32_t sdu_interval_p_to_c;
    uint8_t  worst_case_sca;
    uint8_t  packing;
    uint8_t  framing;
    uint16_t max_transport_latency_c_to_p;  /* milliseconds */
    uint16_t max_transport_latency_p_to_c;
    uint8_t  cis_count;
    hci_cis_params cis[HCI_MAX_CIS_PER_CIG];
} hci_cig_params;

typedef struct {
    uint16_t connection_handle;
    uint16_t interval_min;   /* 1.25 ms units */
    uint16_t interval_max;
    uint16_t max_latency;    /* connection events */
    uint16_t timeout;        /* 10 ms units */
} hci_conn_update_params;

/* A device identity learned from connection events. */
typedef struct {
    uint16_t connection_handle;
    uint8_t  address_type;
    uint8_t  address[6];     /* little-endian, as it appears on the wire */
} hci_connection_info;

/* ---- Overrides: which fields a rule wants changed ---- */

#define OLEA_SET_MAX_TRANSPORT_LATENCY  (1u << 0)
#define OLEA_SET_RTN                    (1u << 1)
#define OLEA_SET_PHY                    (1u << 2)
#define OLEA_SET_MAX_SDU                (1u << 3)
#define OLEA_SET_CONN_INTERVAL          (1u << 4)
#define OLEA_SET_CONN_LATENCY           (1u << 5)
#define OLEA_SET_SUPERVISION_TIMEOUT    (1u << 6)

typedef struct {
    uint32_t fields;   /* bitmask of OLEA_SET_* */

    uint16_t max_transport_latency;  /* ms, 5..4000 */
    uint8_t  rtn;                    /* 0..15 */
    uint8_t  phy;                    /* HCI_PHY_* bitfield */
    uint16_t max_sdu;                /* octets, 0..4095 */

    uint16_t conn_interval_min;      /* 1.25 ms units, 6..3200 */
    uint16_t conn_interval_max;
    uint16_t conn_latency;           /* 0..499 */
    uint16_t supervision_timeout;    /* 10 ms units, 10..3200 */
} olea_overrides;

/* ---- Result of an attempted rewrite ---- */

typedef enum {
    OLEA_UNCHANGED = 0,   /* nothing applied: not our packet, or nothing to do */
    OLEA_REWRITTEN = 1,   /* at least one field changed in place */
    OLEA_MALFORMED = 2,   /* packet failed validation and was left untouched */
    OLEA_REJECTED  = 3    /* override values failed range checks; left untouched */
} olea_result;

/* ---- Parsing ---- */

bool hci_parse_command(uint8_t *buffer, size_t length, hci_command *out);
bool hci_parse_event(const uint8_t *buffer, size_t length, hci_event *out);

bool hci_parse_cig_params(const hci_command *cmd, hci_cig_params *out);
bool hci_parse_conn_update(const hci_command *cmd, hci_conn_update_params *out);

/* Extracts handle and peer address from LE Enhanced Connection Complete. */
bool hci_parse_enhanced_conn_complete(const hci_event *evt, hci_connection_info *out);

/* Extracts the handle that Disconnection Complete refers to. */
bool hci_parse_disconnection_complete(const hci_event *evt, uint16_t *handle_out);

/* ---- Rewriting (in place, never changes packet length) ---- */

/*
 * Applies overrides to LE Set CIG Parameters. Touches every CIS in the CIG,
 * because a stereo stream split across two CIS must stay symmetric.
 */
olea_result hci_rewrite_cig_params(hci_command *cmd, const olea_overrides *ov);

/* Applies overrides to LE Connection Update. */
olea_result hci_rewrite_conn_update(hci_command *cmd, const olea_overrides *ov);

/* ---- Validation ---- */

/* Range-checks an override set against the Core spec. Rules that fail never reach the kernel. */
bool olea_overrides_valid(const olea_overrides *ov);

/* ---- Little-endian helpers (exposed for tests) ---- */

static inline uint16_t hci_read_u16(const uint8_t *p)
{
    return (uint16_t)(p[0] | ((uint16_t)p[1] << 8));
}

static inline uint32_t hci_read_u24(const uint8_t *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16);
}

static inline void hci_write_u16(uint8_t *p, uint16_t value)
{
    p[0] = (uint8_t)(value & 0xFF);
    p[1] = (uint8_t)(value >> 8);
}

#endif /* OLEA_HCI_H */
