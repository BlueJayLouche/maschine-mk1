/* Maschine Mk1 on ESP32-S3 USB host — bring-up spike.
 *
 * Proves the S3 can host the Mk1 (full-speed fallback) and run the protocol:
 * enumerate, handshake, stream pads/knobs/buttons to the serial log, echo
 * pad pressure to pad LEDs. Protocol reference: ../docs/protocol.md and the
 * hardware-verified mk1-protocol Rust crate. The production firmware will be
 * esp-rs reusing that crate; this spike is the USB-host plumbing rehearsal.
 *
 * Wiring: flash/monitor via the UART connector; the Mk1 plugs into the
 * USB-OTG connector (which must source 5V — power the board accordingly).
 */
#include <string.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_log.h"
#include "usb/usb_host.h"
#include "usb/usb_helpers.h"

static const char *TAG = "mk1";

#define MK1_VID 0x17cc
#define MK1_PID 0x0808

#define CMD_GET_DEVICE_INFO 0x01
#define CMD_READ_ERP 0x02
#define CMD_READ_IO 0x04
#define CMD_MIDI_READ 0x06
#define CMD_AUTO_MSG 0x0b
#define CMD_DIMM_LEDS 0x0c

static usb_host_client_handle_t client;
static usb_device_handle_t dev;
static uint8_t dev_addr; /* 0 = nothing pending */

static usb_transfer_t *ep1_in, *ep1_out, *ep4_in, *ctrl;

/* --- EP1 OUT with a small pending queue (callbacks may overlap) --------- */
static struct {
    uint8_t buf[8][40];
    uint8_t len[8];
    uint8_t head, tail;
    bool busy;
} txq;

static void ep1_pump(void)
{
    if (txq.busy || txq.head == txq.tail)
        return;
    uint8_t i = txq.tail % 8;
    memcpy(ep1_out->data_buffer, txq.buf[i], txq.len[i]);
    ep1_out->num_bytes = txq.len[i];
    txq.tail++;
    txq.busy = true;
    if (usb_host_transfer_submit(ep1_out) != ESP_OK) {
        ESP_LOGE(TAG, "EP1 OUT submit failed");
        txq.busy = false;
    }
}

static void ep1_send(const uint8_t *data, uint8_t len)
{
    if ((uint8_t)(txq.head - txq.tail) >= 8) {
        ESP_LOGW(TAG, "EP1 queue full, dropping cmd 0x%02x", data[0]);
        return;
    }
    uint8_t i = txq.head % 8;
    memcpy(txq.buf[i], data, len);
    txq.len[i] = len;
    txq.head++;
    ep1_pump();
}

static void ep1_out_cb(usb_transfer_t *t)
{
    txq.busy = false;
    ep1_pump();
}

/* --- LEDs ---------------------------------------------------------------- */
static uint8_t led_state[64];
static bool led_dirty[2];

/* printed pad number 1-16 -> LED position (kernel table, hw-verified) */
static const uint8_t pad_led[16] = {3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12};
#define LED_BACKLIGHT 59

static void led_set(uint8_t pos, uint8_t v)
{
    v = v > 63 ? 63 : v;
    if (led_state[pos] != v) {
        led_state[pos] = v;
        led_dirty[pos / 32] = true;
    }
}

static void led_flush(void)
{
    static const uint8_t bank_id[2] = {0x00, 0x1e};
    for (int b = 0; b < 2; b++) {
        if (!led_dirty[b])
            continue;
        led_dirty[b] = false;
        uint8_t msg[34] = {CMD_DIMM_LEDS, bank_id[b]};
        memcpy(msg + 2, led_state + b * 32, 32);
        ep1_send(msg, sizeof(msg));
    }
}

/* --- input decode (see docs/protocol.md) --------------------------------- */
static int decode_erp(uint8_t a8, uint8_t b8)
{
    const int LOW = -7, HIGH = 268, RANGE = HIGH - LOW;
    int a = a8, b = b8, mid = (HIGH + LOW) / 2;
    int wb = abs(mid - a) - (RANGE / 2 - 100) / 2;
    wb = wb < 0 ? 0 : (wb > 100 ? 100 : wb);
    int wa = 100 - wb;
    int pb, pa;
    if (a < mid) {
        pb = b - LOW + RANGE / 2 + RANGE;
        if (pb >= RANGE * 2)
            pb -= RANGE * 2;
    } else {
        pb = HIGH - b + RANGE / 2;
    }
    pa = (b > mid) ? a - LOW : HIGH - a + RANGE;
    int ret = (pa * wa + pb * wb) * 10 / (RANGE * 2);
    if (ret < 0)
        ret += 1000;
    if (ret >= 1000)
        ret -= 1000;
    return ret;
}

/* knob (a,b) byte offsets in the ERP payload, knobs 1-8 then vol/tempo/swing */
static const uint8_t erp_off[11][2] = {{21, 20}, {15, 14}, {9, 8}, {3, 2},
    {19, 18}, {13, 12}, {7, 6}, {1, 0}, {17, 16}, {11, 10}, {5, 4}};
static const char *knob_name[11] = {"knob1", "knob2", "knob3", "knob4",
    "knob5", "knob6", "knob7", "knob8", "volume", "tempo", "swing"};

/* READ_IO bit order (bit 8 reserved; softkeys run right-to-left) */
static const char *button_name[42] = {"mute", "solo", "select", "duplicate",
    "navigate", "pad_mode", "pattern", "scene", NULL, "rec", "erase", "shift",
    "grid", "step_right", "step_left", "restart", "group_e", "group_f",
    "group_g", "group_h", "group_d", "group_c", "group_b", "group_a",
    "control", "browse", "left", "snap", "autowrite", "right", "sampling",
    "step", "softkey8", "softkey7", "softkey6", "softkey5", "softkey4",
    "softkey3", "softkey2", "softkey1", "note_repeat", "play"};

static int knob_prev[11] = {-1};
static uint64_t buttons_prev;
static uint16_t pad_prev[16];

static void handle_ep1_msg(const uint8_t *b, int n)
{
    if (n < 1)
        return;
    switch (b[0]) {
    case CMD_GET_DEVICE_INFO:
        if (n >= 14) {
            ESP_LOGI(TAG, "Maschine Mk1 says hello: firmware %d", b[1] | (b[2] << 8));
            static const uint8_t am[] = {CMD_AUTO_MSG, 1, 10, 5};
            ep1_send(am, sizeof(am));
            led_set(LED_BACKLIGHT, 0x5c);
            led_flush();
            usb_host_transfer_submit(ep4_in);
        }
        break;
    case CMD_READ_ERP:
        if (n >= 23) {
            for (int i = 0; i < 11; i++) {
                int v = decode_erp(b[1 + erp_off[i][0]], b[1 + erp_off[i][1]]);
                if (knob_prev[i] >= 0 && abs(v - knob_prev[i]) > 4)
                    ESP_LOGI(TAG, "%s = %d", knob_name[i], v);
                knob_prev[i] = v;
            }
        }
        break;
    case CMD_READ_IO: {
        uint64_t now = 0;
        for (int i = 1; i < n && i <= 6; i++)
            now |= (uint64_t)b[i] << ((i - 1) * 8);
        uint64_t diff = now ^ buttons_prev;
        for (int i = 0; i < 42; i++)
            if ((diff >> i) & 1 && button_name[i])
                ESP_LOGI(TAG, "button %s %s", button_name[i],
                         (now >> i) & 1 ? "down" : "up");
        buttons_prev = now;
        break;
    }
    case CMD_MIDI_READ:
        ESP_LOGI(TAG, "DIN MIDI in, %d bytes", n >= 3 ? b[2] : 0);
        break;
    }
}

static void ep1_in_cb(usb_transfer_t *t)
{
    if (t->status == USB_TRANSFER_STATUS_COMPLETED)
        handle_ep1_msg(t->data_buffer, t->actual_num_bytes);
    else if (t->status != USB_TRANSFER_STATUS_TIMED_OUT)
        ESP_LOGW(TAG, "EP1 IN status %d", t->status);
    if (dev)
        usb_host_transfer_submit(t);
}

static void ep4_in_cb(usb_transfer_t *t)
{
    if (t->status == USB_TRANSFER_STATUS_COMPLETED) {
        const uint8_t *b = t->data_buffer;
        for (int i = 0; i + 1 < t->actual_num_bytes; i += 2) {
            uint16_t w = b[i] | (b[i + 1] << 8);
            uint8_t raw = w >> 12;
            uint16_t p = w & 0xfff;
            if (p < 6)
                p = 0;
            /* raw ids are top-left origin; printed numbers start bottom-left */
            uint8_t phys = (3 - raw / 4) * 4 + raw % 4 + 1;
            uint16_t d = p > pad_prev[raw] ? p - pad_prev[raw] : pad_prev[raw] - p;
            if (d >= 128 || (p == 0 && pad_prev[raw] != 0)) {
                ESP_LOGI(TAG, "pad %d pressure %d", phys, p);
                pad_prev[raw] = p;
            }
            led_set(pad_led[phys - 1], p >> 6);
        }
        led_flush();
    }
    if (dev)
        usb_host_transfer_submit(t);
}

static void ctrl_cb(usb_transfer_t *t)
{
    ESP_LOGI(TAG, "SET_INTERFACE alt 1 done, starting session");
    usb_host_transfer_submit(ep1_in);
    static const uint8_t gdi[] = {CMD_GET_DEVICE_INFO};
    ep1_send(gdi, sizeof(gdi));
}

static void attach_mk1(void)
{
    usb_host_transfer_alloc(64, 0, &ep1_in);
    usb_host_transfer_alloc(64, 0, &ep1_out);
    usb_host_transfer_alloc(512, 0, &ep4_in);
    usb_host_transfer_alloc(sizeof(usb_setup_packet_t), 0, &ctrl);

    ep1_in->device_handle = dev;
    ep1_in->bEndpointAddress = 0x81;
    ep1_in->num_bytes = 64;
    ep1_in->callback = ep1_in_cb;

    ep1_out->device_handle = dev;
    ep1_out->bEndpointAddress = 0x01;
    ep1_out->callback = ep1_out_cb;

    ep4_in->device_handle = dev;
    ep4_in->bEndpointAddress = 0x84;
    ep4_in->num_bytes = 512;
    ep4_in->callback = ep4_in_cb;

    /* At full speed the Mk1 may present a different config descriptor than
     * the high-speed layout in docs/protocol.md (512-byte bulk EPs are
     * illegal at FS) — dump what it actually offers before claiming. */
    const usb_config_desc_t *cfg;
    if (usb_host_get_active_config_descriptor(dev, &cfg) == ESP_OK) {
        usb_print_config_descriptor(cfg, NULL);
        /* FS fallback: the Mk1 serves its HS descriptors verbatim — 512-byte
         * bulk MPS is illegal at full speed and the DWC host rejects it. The
         * wire MPS at FS is 64 by physics, so patch the host's cached
         * descriptor (it's RAM, read from the device at enumeration) so the
         * claim/pipe-alloc paths see the truth. */
        int off = 0;
        const usb_standard_desc_t *d = (const usb_standard_desc_t *)cfg;
        while ((d = usb_parse_next_descriptor_of_type(
                    d, cfg->wTotalLength, USB_B_DESCRIPTOR_TYPE_ENDPOINT, &off)) != NULL) {
            usb_ep_desc_t *ep = (usb_ep_desc_t *)d;
            if (ep->wMaxPacketSize > 64) {
                ESP_LOGW(TAG, "clamping EP 0x%02x MPS %d -> 64",
                         ep->bEndpointAddress, ep->wMaxPacketSize);
                ep->wMaxPacketSize = 64;
            }
        }
    }

    esp_err_t err = usb_host_interface_claim(client, dev, 0, 1);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "claim iface 0 alt 1: %s — trying alt 0", esp_err_to_name(err));
        err = usb_host_interface_claim(client, dev, 0, 0);
    }
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "claim iface 0 alt 0 also failed: %s — descriptor above is the clue",
                 esp_err_to_name(err));
        return;
    }

    /* SET_INTERFACE(0, alt 1) — claim alone doesn't send it */
    usb_setup_packet_t *s = (usb_setup_packet_t *)ctrl->data_buffer;
    s->bmRequestType = 0x01;
    s->bRequest = 11; /* SET_INTERFACE */
    s->wValue = 1;
    s->wIndex = 0;
    s->wLength = 0;
    ctrl->num_bytes = sizeof(usb_setup_packet_t);
    ctrl->callback = ctrl_cb;
    ctrl->device_handle = dev;
    ctrl->bEndpointAddress = 0;
    ESP_ERROR_CHECK(usb_host_transfer_submit_control(client, ctrl));
}

static void client_cb(const usb_host_client_event_msg_t *msg, void *arg)
{
    if (msg->event == USB_HOST_CLIENT_EVENT_NEW_DEV) {
        dev_addr = msg->new_dev.address;
    } else if (msg->event == USB_HOST_CLIENT_EVENT_DEV_GONE) {
        ESP_LOGW(TAG, "device gone");
        if (dev) {
            usb_host_interface_release(client, dev, 0);
            usb_host_device_close(client, dev);
            dev = NULL;
        }
    }
}

static void host_lib_task(void *arg)
{
    while (1) {
        uint32_t flags;
        usb_host_lib_handle_events(portMAX_DELAY, &flags);
        if (flags & USB_HOST_LIB_EVENT_FLAGS_NO_CLIENTS)
            usb_host_device_free_all();
    }
}

void app_main(void)
{
    const usb_host_config_t host_cfg = {.intr_flags = ESP_INTR_FLAG_LEVEL1};
    ESP_ERROR_CHECK(usb_host_install(&host_cfg));
    xTaskCreate(host_lib_task, "usbh", 4096, NULL, 10, NULL);

    const usb_host_client_config_t client_cfg = {
        .is_synchronous = false,
        .max_num_event_msg = 8,
        .async = {.client_event_callback = client_cb},
    };
    ESP_ERROR_CHECK(usb_host_client_register(&client_cfg, &client));
    ESP_LOGI(TAG, "USB host ready — plug the Maschine Mk1 into the OTG port");

    while (1) {
        usb_host_client_handle_events(client, pdMS_TO_TICKS(100));
        if (dev_addr && !dev) {
            uint8_t addr = dev_addr;
            dev_addr = 0;
            if (usb_host_device_open(client, addr, &dev) != ESP_OK)
                continue;
            const usb_device_desc_t *dd;
            usb_host_get_device_descriptor(dev, &dd);
            ESP_LOGI(TAG, "device %04x:%04x attached", dd->idVendor, dd->idProduct);
            if (dd->idVendor == MK1_VID && dd->idProduct == MK1_PID) {
                attach_mk1();
            } else {
                ESP_LOGI(TAG, "not a Maschine Mk1, ignoring");
                usb_host_device_close(client, dev);
                dev = NULL;
            }
        }
    }
}
