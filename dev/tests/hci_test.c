/*
 * User-mode tests for the HCI layer. Builds with plain MSVC, no WDK needed,
 * so the packet logic can be verified long before any driver is signed.
 *
 * The CIG fixture is deliberately the two-CIS stereo layout that the JBL
 * Tune 780NC negotiates - the configuration whose setup intermittently fails.
 */

#include <stdio.h>
#include <string.h>
#include "../src/hci/hci.h"

static int failures = 0;
static int checks = 0;

static void check(bool condition, const char *what)
{
    checks++;
    if (!condition) {
        failures++;
        printf("  FAIL  %s\n", what);
    }
}

/* LE Set CIG Parameters, CIG 1, 10 ms SDU interval, two CIS (left + right). */
static const uint8_t cig_two_cis[] = {
    0x62, 0x20,             /* opcode 0x2062 */
    0x21,                   /* param length 33 */
    0x01,                   /* CIG_ID */
    0x10, 0x27, 0x00,       /* SDU_Interval_C_To_P = 10000 us */
    0x10, 0x27, 0x00,       /* SDU_Interval_P_To_C = 10000 us */
    0x00,                   /* Worst_Case_SCA */
    0x00,                   /* Packing = sequential */
    0x00,                   /* Framing = unframed */
    0x0A, 0x00,             /* Max_Transport_Latency_C_To_P = 10 ms */
    0x0A, 0x00,             /* Max_Transport_Latency_P_To_C = 10 ms */
    0x02,                   /* CIS_Count = 2 */
    /* CIS 0 - left */
    0x00, 0x64, 0x00, 0x00, 0x00, 0x02, 0x02, 0x02, 0x02,
    /* CIS 1 - right */
    0x01, 0x64, 0x00, 0x00, 0x00, 0x02, 0x02, 0x02, 0x02,
};

/* LE Connection Update: handle 0x0040, 30-45 ms interval, latency 0, timeout 5 s. */
static const uint8_t conn_update[] = {
    0x13, 0x20,             /* opcode 0x2013 */
    0x0E,                   /* param length 14 */
    0x40, 0x00,             /* Connection_Handle */
    0x18, 0x00,             /* Interval_Min = 24 (30 ms) */
    0x24, 0x00,             /* Interval_Max = 36 (45 ms) */
    0x00, 0x00,             /* Max_Latency */
    0xF4, 0x01,             /* Timeout = 500 (5 s) */
    0x00, 0x00, 0x00, 0x00, /* CE lengths */
};

/* LE Enhanced Connection Complete for 7C:FE:62:72:B4:9A on handle 0x0040. */
static const uint8_t enhanced_conn_complete[] = {
    0x3E, 0x1F,             /* LE Meta, 31 params */
    0x0A,                   /* subevent */
    0x00,                   /* status = success */
    0x40, 0x00,             /* handle */
    0x00,                   /* role = central */
    0x00,                   /* peer address type = public */
    0x9A, 0xB4, 0x72, 0x62, 0xFE, 0x7C,   /* peer address, little-endian */
    0, 0, 0, 0, 0, 0,       /* local RPA */
    0, 0, 0, 0, 0, 0,       /* peer RPA */
    0x18, 0x00,             /* conn interval */
    0x00, 0x00,             /* latency */
    0xF4, 0x01,             /* timeout */
    0x00,                   /* clock accuracy */
};

static void test_parse_cig(void)
{
    printf("parse LE Set CIG Parameters (two CIS stereo)\n");

    uint8_t buffer[sizeof cig_two_cis];
    memcpy(buffer, cig_two_cis, sizeof buffer);

    hci_command cmd;
    check(hci_parse_command(buffer, sizeof buffer, &cmd), "command header parses");
    check(cmd.opcode == HCI_OP_LE_SET_CIG_PARAMETERS, "opcode is Set CIG Parameters");

    hci_cig_params cig;
    check(hci_parse_cig_params(&cmd, &cig), "CIG body parses");
    check(cig.cig_id == 1, "CIG id");
    check(cig.sdu_interval_c_to_p == 10000, "SDU interval decodes as 10000 us");
    check(cig.max_transport_latency_c_to_p == 10, "transport latency 10 ms");
    check(cig.cis_count == 2, "two CIS present");
    check(cig.cis[0].cis_id == 0 && cig.cis[1].cis_id == 1, "CIS ids 0 and 1");
    check(cig.cis[0].max_sdu_c_to_p == 100, "CIS 0 max SDU 100 octets");
    check(cig.cis[1].rtn_c_to_p == 2, "CIS 1 RTN 2");
}

static void test_rewrite_cig(void)
{
    printf("rewrite CIG: low-latency profile\n");

    uint8_t buffer[sizeof cig_two_cis];
    memcpy(buffer, cig_two_cis, sizeof buffer);

    hci_command cmd;
    hci_parse_command(buffer, sizeof buffer, &cmd);

    olea_overrides ov;
    memset(&ov, 0, sizeof ov);
    ov.fields = OLEA_SET_MAX_TRANSPORT_LATENCY | OLEA_SET_RTN | OLEA_SET_MAX_SDU;
    ov.max_transport_latency = 8;
    ov.rtn = 1;
    ov.max_sdu = 120;   /* 48_4: 120 octets at 10 ms = 96 kbps per channel */

    check(hci_rewrite_cig_params(&cmd, &ov) == OLEA_REWRITTEN, "rewrite reports success");
    check(buffer[2] == 0x21, "packet length unchanged");

    hci_cig_params cig;
    check(hci_parse_cig_params(&cmd, &cig), "rewritten packet still parses");
    check(cig.max_transport_latency_c_to_p == 8, "transport latency now 8 ms");
    check(cig.max_transport_latency_p_to_c == 8, "both directions updated");
    check(cig.cis_count == 2, "CIS count untouched");

    check(cig.cis[0].rtn_c_to_p == 1 && cig.cis[1].rtn_c_to_p == 1, "RTN applied to every CIS");
    check(cig.cis[0].max_sdu_c_to_p == 120 && cig.cis[1].max_sdu_c_to_p == 120,
          "max SDU applied to every CIS");

    /* The unused peripheral-to-central direction was zero and must stay zero. */
    check(cig.cis[0].max_sdu_p_to_c == 0, "unused direction left at zero");
}

static void test_malformed_cig_untouched(void)
{
    printf("malformed CIG is forwarded untouched\n");

    uint8_t buffer[sizeof cig_two_cis];
    memcpy(buffer, cig_two_cis, sizeof buffer);
    buffer[17] = 0x05;   /* claim five CIS while carrying two */

    uint8_t before[sizeof cig_two_cis];
    memcpy(before, buffer, sizeof buffer);

    hci_command cmd;
    hci_parse_command(buffer, sizeof buffer, &cmd);

    olea_overrides ov;
    memset(&ov, 0, sizeof ov);
    ov.fields = OLEA_SET_RTN;
    ov.rtn = 1;

    check(hci_rewrite_cig_params(&cmd, &ov) == OLEA_MALFORMED, "rewrite refuses the packet");
    check(memcmp(buffer, before, sizeof buffer) == 0, "not a single byte changed");
}

static void test_rewrite_conn_update(void)
{
    printf("rewrite LE Connection Update: gamepad latency\n");

    uint8_t buffer[sizeof conn_update];
    memcpy(buffer, conn_update, sizeof buffer);

    hci_command cmd;
    hci_parse_command(buffer, sizeof buffer, &cmd);

    olea_overrides ov;
    memset(&ov, 0, sizeof ov);
    ov.fields = OLEA_SET_CONN_INTERVAL | OLEA_SET_SUPERVISION_TIMEOUT;
    ov.conn_interval_min = 6;    /* 7.5 ms */
    ov.conn_interval_max = 6;
    ov.supervision_timeout = 100; /* 1 s */

    check(hci_rewrite_conn_update(&cmd, &ov) == OLEA_REWRITTEN, "rewrite reports success");
    check(buffer[2] == 0x0E, "packet length unchanged");

    hci_conn_update_params parsed;
    check(hci_parse_conn_update(&cmd, &parsed), "rewritten packet still parses");
    check(parsed.interval_min == 6 && parsed.interval_max == 6, "interval now 7.5 ms");
    check(parsed.timeout == 100, "supervision timeout applied");
    check(parsed.connection_handle == 0x0040, "handle untouched");
}

static void test_validation_rejects_unstable_link(void)
{
    printf("validation rejects a link that would drop\n");

    olea_overrides ov;
    memset(&ov, 0, sizeof ov);

    /* Timeout must exceed (1 + latency) * interval_max * 2. 100*4 = 400 > 6 -> fine. */
    ov.fields = OLEA_SET_CONN_INTERVAL | OLEA_SET_SUPERVISION_TIMEOUT;
    ov.conn_interval_min = 6;
    ov.conn_interval_max = 6;
    ov.supervision_timeout = 100;
    check(olea_overrides_valid(&ov), "sane combination accepted");

    /* 10*4 = 40, versus (1+499)*6 = 3000 -> must be rejected. */
    ov.fields |= OLEA_SET_CONN_LATENCY;
    ov.conn_latency = 499;
    ov.supervision_timeout = 10;
    check(!olea_overrides_valid(&ov), "timeout too short for the latency is rejected");

    /* Out-of-range values. */
    memset(&ov, 0, sizeof ov);
    ov.fields = OLEA_SET_RTN;
    ov.rtn = 16;
    check(!olea_overrides_valid(&ov), "RTN above 15 rejected");

    memset(&ov, 0, sizeof ov);
    ov.fields = OLEA_SET_MAX_TRANSPORT_LATENCY;
    ov.max_transport_latency = 4;
    check(!olea_overrides_valid(&ov), "transport latency below 5 ms rejected");

    memset(&ov, 0, sizeof ov);
    ov.fields = OLEA_SET_PHY;
    ov.phy = 0x08;
    check(!olea_overrides_valid(&ov), "undefined PHY bit rejected");
}

static void test_rejected_override_leaves_packet_alone(void)
{
    printf("invalid override never modifies a packet\n");

    uint8_t buffer[sizeof cig_two_cis];
    memcpy(buffer, cig_two_cis, sizeof buffer);

    uint8_t before[sizeof cig_two_cis];
    memcpy(before, buffer, sizeof buffer);

    hci_command cmd;
    hci_parse_command(buffer, sizeof buffer, &cmd);

    olea_overrides ov;
    memset(&ov, 0, sizeof ov);
    ov.fields = OLEA_SET_RTN;
    ov.rtn = 200;

    check(hci_rewrite_cig_params(&cmd, &ov) == OLEA_REJECTED, "rewrite reports rejection");
    check(memcmp(buffer, before, sizeof buffer) == 0, "packet untouched");
}

static void test_connection_tracking(void)
{
    printf("connection tracking maps handle to address\n");

    hci_event evt;
    check(hci_parse_event(enhanced_conn_complete, sizeof enhanced_conn_complete, &evt),
          "event parses");
    check(evt.event_code == HCI_EVT_LE_META, "LE meta event");
    check(evt.subevent == HCI_SUBEVT_LE_ENHANCED_CONN_COMPLETE, "enhanced connection complete");

    hci_connection_info info;
    check(hci_parse_enhanced_conn_complete(&evt, &info), "connection info extracted");
    check(info.connection_handle == 0x0040, "handle 0x0040");

    /* Wire order is little-endian; the display form is 7C:FE:62:72:B4:9A. */
    const uint8_t expected[6] = { 0x9A, 0xB4, 0x72, 0x62, 0xFE, 0x7C };
    check(memcmp(info.address, expected, 6) == 0, "peer address matches");
}

static void test_non_matching_packets_ignored(void)
{
    printf("unrelated packets are ignored\n");

    uint8_t buffer[sizeof conn_update];
    memcpy(buffer, conn_update, sizeof buffer);

    hci_command cmd;
    hci_parse_command(buffer, sizeof buffer, &cmd);

    olea_overrides ov;
    memset(&ov, 0, sizeof ov);
    ov.fields = OLEA_SET_RTN;
    ov.rtn = 1;

    /* A CIG rewriter handed a Connection Update must do nothing at all. */
    check(hci_rewrite_cig_params(&cmd, &ov) == OLEA_UNCHANGED, "CIG rewriter ignores conn update");

    /* Truncated buffers must fail parsing rather than read past the end. */
    hci_command truncated;
    check(!hci_parse_command(buffer, 2, &truncated), "two-byte buffer rejected");
    check(!hci_parse_command(buffer, sizeof buffer - 1, &truncated), "short buffer rejected");
}

int main(void)
{
    printf("HCI layer tests\n");
    printf("========================================\n");

    test_parse_cig();
    test_rewrite_cig();
    test_malformed_cig_untouched();
    test_rewrite_conn_update();
    test_validation_rejects_unstable_link();
    test_rejected_override_leaves_packet_alone();
    test_connection_tracking();
    test_non_matching_packets_ignored();

    printf("========================================\n");
    printf("%d checks, %d failures\n", checks, failures);
    return failures == 0 ? 0 : 1;
}
