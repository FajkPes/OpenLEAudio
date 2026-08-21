/*
 * HCI packet inspection and rewriting. See hci.h for the contract.
 *
 * Rule that governs this file: a packet is either fully understood and rewritten,
 * or left byte-for-byte alone. There is no partial application - a CIG whose CIS
 * array does not add up is forwarded untouched rather than half-modified.
 */

#include "hci.h"

/* ---- Parsing ---- */

bool hci_parse_command(uint8_t *buffer, size_t length, hci_command *out)
{
    if (!buffer || !out || length < 3) return false;

    uint8_t param_len = buffer[2];
    if ((size_t)param_len + 3u != length) return false;  /* declared length must match exactly */

    out->opcode = hci_read_u16(buffer);
    out->param_len = param_len;
    out->params = buffer + 3;
    return true;
}

bool hci_parse_event(const uint8_t *buffer, size_t length, hci_event *out)
{
    if (!buffer || !out || length < 2) return false;

    uint8_t param_len = buffer[1];
    if ((size_t)param_len + 2u != length) return false;

    out->event_code = buffer[0];
    out->param_len = param_len;
    out->params = buffer + 2;
    out->subevent = 0;

    if (out->event_code == HCI_EVT_LE_META) {
        if (param_len < 1) return false;
        out->subevent = out->params[0];
    }

    return true;
}

bool hci_parse_cig_params(const hci_command *cmd, hci_cig_params *out)
{
    if (!cmd || !out) return false;
    if (cmd->opcode != HCI_OP_LE_SET_CIG_PARAMETERS) return false;
    if (cmd->param_len < HCI_CIG_HEADER_LEN) return false;

    const uint8_t *p = cmd->params;
    uint8_t cis_count = p[14];

    if (cis_count > HCI_MAX_CIS_PER_CIG) return false;

    /* The CIS array must account for the remaining bytes exactly. */
    size_t expected = HCI_CIG_HEADER_LEN + (size_t)cis_count * HCI_CIG_CIS_ENTRY_LEN;
    if ((size_t)cmd->param_len != expected) return false;

    out->cig_id = p[0];
    out->sdu_interval_c_to_p = hci_read_u24(p + 1);
    out->sdu_interval_p_to_c = hci_read_u24(p + 4);
    out->worst_case_sca = p[7];
    out->packing = p[8];
    out->framing = p[9];
    out->max_transport_latency_c_to_p = hci_read_u16(p + 10);
    out->max_transport_latency_p_to_c = hci_read_u16(p + 12);
    out->cis_count = cis_count;

    for (uint8_t i = 0; i < cis_count; i++) {
        const uint8_t *e = p + HCI_CIG_HEADER_LEN + (size_t)i * HCI_CIG_CIS_ENTRY_LEN;
        out->cis[i].cis_id         = e[0];
        out->cis[i].max_sdu_c_to_p = hci_read_u16(e + 1);
        out->cis[i].max_sdu_p_to_c = hci_read_u16(e + 3);
        out->cis[i].phy_c_to_p     = e[5];
        out->cis[i].phy_p_to_c     = e[6];
        out->cis[i].rtn_c_to_p     = e[7];
        out->cis[i].rtn_p_to_c     = e[8];
    }

    return true;
}

bool hci_parse_conn_update(const hci_command *cmd, hci_conn_update_params *out)
{
    if (!cmd || !out) return false;
    if (cmd->opcode != HCI_OP_LE_CONNECTION_UPDATE) return false;
    if (cmd->param_len != 14) return false;

    const uint8_t *p = cmd->params;
    out->connection_handle = hci_read_u16(p) & 0x0FFF;  /* upper 4 bits reserved */
    out->interval_min = hci_read_u16(p + 2);
    out->interval_max = hci_read_u16(p + 4);
    out->max_latency  = hci_read_u16(p + 6);
    out->timeout      = hci_read_u16(p + 8);
    return true;
}

bool hci_parse_enhanced_conn_complete(const hci_event *evt, hci_connection_info *out)
{
    if (!evt || !out) return false;
    if (evt->event_code != HCI_EVT_LE_META) return false;
    if (evt->subevent != HCI_SUBEVT_LE_ENHANCED_CONN_COMPLETE) return false;

    /* subevent(1) status(1) handle(2) role(1) addr_type(1) addr(6) ... */
    if (evt->param_len < 12) return false;

    const uint8_t *p = evt->params;
    if (p[1] != 0x00) return false;  /* failed connection: nothing to record */

    out->connection_handle = hci_read_u16(p + 2) & 0x0FFF;
    out->address_type = p[5];
    for (int i = 0; i < 6; i++) out->address[i] = p[6 + i];
    return true;
}

bool hci_parse_disconnection_complete(const hci_event *evt, uint16_t *handle_out)
{
    if (!evt || !handle_out) return false;
    if (evt->event_code != HCI_EVT_DISCONNECTION_COMPLETE) return false;
    if (evt->param_len < 4) return false;

    /* status(1) handle(2) reason(1) */
    *handle_out = hci_read_u16(evt->params + 1) & 0x0FFF;
    return true;
}

/* ---- Validation ---- */

bool olea_overrides_valid(const olea_overrides *ov)
{
    if (!ov) return false;
    if (ov->fields == 0) return true;  /* a no-op rule is valid, just pointless */

    if (ov->fields & OLEA_SET_MAX_TRANSPORT_LATENCY) {
        if (ov->max_transport_latency < 5 || ov->max_transport_latency > 4000) return false;
    }

    if (ov->fields & OLEA_SET_RTN) {
        if (ov->rtn > 15) return false;
    }

    if (ov->fields & OLEA_SET_PHY) {
        if (ov->phy == 0 || (ov->phy & ~(HCI_PHY_1M | HCI_PHY_2M | HCI_PHY_CODED)) != 0) return false;
    }

    if (ov->fields & OLEA_SET_MAX_SDU) {
        if (ov->max_sdu > 0x0FFF) return false;
    }

    if (ov->fields & OLEA_SET_CONN_INTERVAL) {
        if (ov->conn_interval_min < 6 || ov->conn_interval_min > 3200) return false;
        if (ov->conn_interval_max < 6 || ov->conn_interval_max > 3200) return false;
        if (ov->conn_interval_min > ov->conn_interval_max) return false;
    }

    if (ov->fields & OLEA_SET_CONN_LATENCY) {
        if (ov->conn_latency > 499) return false;
    }

    if (ov->fields & OLEA_SET_SUPERVISION_TIMEOUT) {
        if (ov->supervision_timeout < 10 || ov->supervision_timeout > 3200) return false;
    }

    /*
     * Core spec constraint, and the one that actually bites: the supervision
     * timeout must exceed (1 + latency) * interval_max * 2, in matching units.
     * Timeout is 10 ms, interval is 1.25 ms, so the comparison reduces to
     *   timeout * 4 > (1 + latency) * interval_max
     * A rule that violates this produces a link that drops under load, which is
     * exactly the failure we are trying to fix - so it never reaches the kernel.
     */
    if ((ov->fields & OLEA_SET_SUPERVISION_TIMEOUT) &&
        (ov->fields & OLEA_SET_CONN_INTERVAL)) {
        uint32_t latency = (ov->fields & OLEA_SET_CONN_LATENCY) ? ov->conn_latency : 0;
        uint32_t left = (uint32_t)ov->supervision_timeout * 4u;
        uint32_t right = (latency + 1u) * (uint32_t)ov->conn_interval_max;
        if (left <= right) return false;
    }

    return true;
}

/* ---- Rewriting ---- */

olea_result hci_rewrite_cig_params(hci_command *cmd, const olea_overrides *ov)
{
    if (!cmd || !ov) return OLEA_UNCHANGED;
    if (cmd->opcode != HCI_OP_LE_SET_CIG_PARAMETERS) return OLEA_UNCHANGED;

    uint32_t relevant = OLEA_SET_MAX_TRANSPORT_LATENCY | OLEA_SET_RTN |
                        OLEA_SET_PHY | OLEA_SET_MAX_SDU;
    if ((ov->fields & relevant) == 0) return OLEA_UNCHANGED;

    if (!olea_overrides_valid(ov)) return OLEA_REJECTED;

    /* Validate the whole packet before touching a single byte. */
    hci_cig_params parsed;
    if (!hci_parse_cig_params(cmd, &parsed)) return OLEA_MALFORMED;

    uint8_t *p = cmd->params;
    bool changed = false;

    if (ov->fields & OLEA_SET_MAX_TRANSPORT_LATENCY) {
        hci_write_u16(p + 10, ov->max_transport_latency);
        hci_write_u16(p + 12, ov->max_transport_latency);
        changed = true;
    }

    /*
     * Every CIS in the CIG gets the same treatment. For a stereo stream carried
     * over two CIS, leaving them asymmetric would skew the channels against each
     * other, so partial application is not an option here either.
     */
    for (uint8_t i = 0; i < parsed.cis_count; i++) {
        uint8_t *e = p + HCI_CIG_HEADER_LEN + (size_t)i * HCI_CIG_CIS_ENTRY_LEN;

        if (ov->fields & OLEA_SET_MAX_SDU) {
            /* Only resize a direction that is actually in use; 0 means unused. */
            if (hci_read_u16(e + 1) != 0) hci_write_u16(e + 1, ov->max_sdu);
            if (hci_read_u16(e + 3) != 0) hci_write_u16(e + 3, ov->max_sdu);
            changed = true;
        }

        if (ov->fields & OLEA_SET_PHY) {
            e[5] = ov->phy;
            e[6] = ov->phy;
            changed = true;
        }

        if (ov->fields & OLEA_SET_RTN) {
            e[7] = ov->rtn;
            e[8] = ov->rtn;
            changed = true;
        }
    }

    return changed ? OLEA_REWRITTEN : OLEA_UNCHANGED;
}

olea_result hci_rewrite_conn_update(hci_command *cmd, const olea_overrides *ov)
{
    if (!cmd || !ov) return OLEA_UNCHANGED;
    if (cmd->opcode != HCI_OP_LE_CONNECTION_UPDATE) return OLEA_UNCHANGED;

    uint32_t relevant = OLEA_SET_CONN_INTERVAL | OLEA_SET_CONN_LATENCY |
                        OLEA_SET_SUPERVISION_TIMEOUT;
    if ((ov->fields & relevant) == 0) return OLEA_UNCHANGED;

    if (!olea_overrides_valid(ov)) return OLEA_REJECTED;

    hci_conn_update_params parsed;
    if (!hci_parse_conn_update(cmd, &parsed)) return OLEA_MALFORMED;

    uint8_t *p = cmd->params;
    bool changed = false;

    if (ov->fields & OLEA_SET_CONN_INTERVAL) {
        hci_write_u16(p + 2, ov->conn_interval_min);
        hci_write_u16(p + 4, ov->conn_interval_max);
        changed = true;
    }

    if (ov->fields & OLEA_SET_CONN_LATENCY) {
        hci_write_u16(p + 6, ov->conn_latency);
        changed = true;
    }

    if (ov->fields & OLEA_SET_SUPERVISION_TIMEOUT) {
        hci_write_u16(p + 8, ov->supervision_timeout);
        changed = true;
    }

    return changed ? OLEA_REWRITTEN : OLEA_UNCHANGED;
}
