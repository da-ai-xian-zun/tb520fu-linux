// SPDX-License-Identifier: GPL-2.0-only
/*
 * Novatek NT36523 DriverIC panels driver
 *
 * Copyright (c) 2022, 2023 Jianhua Lu <lujianhua000@gmail.com>
 */

#include <linux/backlight.h>
#include <linux/delay.h>
#include <linux/gpio/consumer.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/of_graph.h>
#include <linux/regulator/consumer.h>

#include <video/mipi_display.h>

#include <drm/display/drm_dsc.h>
#include <drm/display/drm_dsc_helper.h>
#include <drm/drm_connector.h>
#include <drm/drm_crtc.h>
#include <drm/drm_mipi_dsi.h>
#include <drm/drm_modes.h>
#include <drm/drm_of.h>
#include <drm/drm_panel.h>
#include <linux/version.h>

#define DSI_NUM_MIN 1

static int nt36523_dcs_write_buf(struct mipi_dsi_device *dsi,
				 const u8 *buf, size_t len)
{
	ssize_t ret;

	ret = mipi_dsi_dcs_write_buffer(dsi, buf, len);

	return ret < 0 ? ret : 0;
}

#define mipi_dsi_dcs_write_seq(dsi, seq...)			\
	do {							\
		static const u8 d[] = { seq };			\
								\
		nt36523_dcs_write_buf(dsi, d, ARRAY_SIZE(d));	\
	} while (0)

#define mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, seq...)		\
	do {								\
		static const u8 d[] = { seq };				\
									\
		nt36523_dcs_write_buf(dsi0, d, ARRAY_SIZE(d));		\
		nt36523_dcs_write_buf(dsi1, d, ARRAY_SIZE(d));		\
	} while (0)

struct panel_info {
	struct drm_panel panel;
	struct mipi_dsi_device *dsi[2];
	const struct panel_desc *desc;
	enum drm_panel_orientation orientation;

	struct gpio_desc *reset_gpio;
	struct backlight_device *backlight;
	struct regulator *vddio;
	struct regulator *enp;
	struct regulator *enn;
	struct drm_dsc_config dsc;
};

struct panel_desc {
	unsigned int width_mm;
	unsigned int height_mm;

	unsigned int bpc;
	unsigned int lanes;
	unsigned long mode_flags;
	enum mipi_dsi_pixel_format format;

	const struct drm_display_mode *modes;
	unsigned int num_modes;
	const struct mipi_dsi_device_info dsi_info;
	int (*init_sequence)(struct panel_info *pinfo);

	bool is_dual_dsi;
	bool has_dcs_backlight;
	bool has_dsc;
	bool display_on_after_dsc;

	unsigned int dsc_pic_width;
	unsigned int dsc_pic_height;
	unsigned int dsc_slice_width;
	unsigned int dsc_slice_height;
	unsigned int dsc_slice_per_pkt;
	unsigned int dsc_first_line_bpg_offset;
};

static inline struct panel_info *to_panel_info(struct drm_panel *panel)
{
	return container_of(panel, struct panel_info, panel);
}

static int elish_boe_init_sequence(struct panel_info *pinfo)
{
	struct mipi_dsi_device *dsi0 = pinfo->dsi[0];
	struct mipi_dsi_device *dsi1 = pinfo->dsi[1];
	/* No datasheet, so write magic init sequence directly */
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb9, 0x05);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x18, 0x40);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb9, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x23);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x00, 0x80);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x01, 0x84);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x05, 0x2d);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x06, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x07, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x08, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x09, 0x45);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x11, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x12, 0x80);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x15, 0x83);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x16, 0x0c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x29, 0x0a);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x30, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x31, 0xfe);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x32, 0xfd);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x33, 0xfb);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x34, 0xf8);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x35, 0xf5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x36, 0xf3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x37, 0xf2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x38, 0xf2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x39, 0xf2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3a, 0xef);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3b, 0xec);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3d, 0xe9);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3f, 0xe5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x40, 0xe5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x41, 0xe5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x2a, 0x13);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x45, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x46, 0xf4);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x47, 0xe7);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x48, 0xda);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x49, 0xcd);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4a, 0xc0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4b, 0xb3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4c, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4d, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4e, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4f, 0x99);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x50, 0x80);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x51, 0x68);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x52, 0x66);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x53, 0x66);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x54, 0x66);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x2b, 0x0e);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x58, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x59, 0xfb);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5a, 0xf7);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5b, 0xf3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5c, 0xef);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5d, 0xe3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5e, 0xda);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5f, 0xd8);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x60, 0xd8);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x61, 0xd8);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x62, 0xcb);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x63, 0xbf);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x64, 0xb3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x65, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x66, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x67, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x2a);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x25, 0x47);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x30, 0x47);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x39, 0x47);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x26);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x19, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1a, 0xe0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1b, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1c, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x2a, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x2b, 0xe0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0xf0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x84, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x85, 0x0c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x51, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x25);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x91, 0x1f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x92, 0x0f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x93, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x94, 0x18);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x95, 0x03);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x96, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb0, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x25);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x19, 0x1f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1b, 0x1b);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x24);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb8, 0x28);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x27);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd0, 0x31);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd1, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd2, 0x30);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd4, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xde, 0x80);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xdf, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x26);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x00, 0x81);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x01, 0xb0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x22);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x9f, 0x50);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x6f, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x70, 0x11);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x73, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x74, 0x49);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x76, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x77, 0x49);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xa0, 0x3f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xa9, 0x50);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xaa, 0x28);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xab, 0x28);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xad, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb8, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb9, 0x49);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xba, 0x49);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbb, 0x49);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbe, 0x04);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbf, 0x49);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc0, 0x04);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc1, 0x59);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc2, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc5, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc6, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc7, 0x48);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xca, 0x43);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xcb, 0x3c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xce, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xcf, 0x43);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd0, 0x3c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd3, 0x43);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd4, 0x3c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd7, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xdc, 0x43);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xdd, 0x3c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xe1, 0x43);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xe2, 0x3c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf2, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf3, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf4, 0x48);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x25);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x13, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x14, 0x23);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbc, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbd, 0x23);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x2a);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x97, 0x3c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x98, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x99, 0x95);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x9a, 0x03);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x9b, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x9c, 0x0b);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x9d, 0x0a);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x9e, 0x90);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x22);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x9f, 0x50);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x23);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xa3, 0x50);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0xe0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x14, 0x60);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x16, 0xc0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4f, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0xf0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3a, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0xd0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x02, 0xaf);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x09, 0xee);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1c, 0x99);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1d, 0x09);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x51, 0x0f, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x53, 0x2c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x35, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbb, 0x13);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3b, 0x03, 0xac, 0x1a, 0x04, 0x04);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x11);
	msleep(70);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x29);

	return 0;
}

static int elish_csot_init_sequence(struct panel_info *pinfo)
{
	struct mipi_dsi_device *dsi0 = pinfo->dsi[0];
	struct mipi_dsi_device *dsi1 = pinfo->dsi[1];
	/* No datasheet, so write magic init sequence directly */
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb9, 0x05);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x18, 0x40);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb9, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0xd0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x02, 0xaf);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x00, 0x30);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x09, 0xee);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1c, 0x99);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1d, 0x09);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0xf0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3a, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0xe0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4f, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x58, 0x40);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x35, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x23);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x00, 0x80);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x01, 0x84);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x05, 0x2d);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x06, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x07, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x08, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x09, 0x45);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x11, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x12, 0x80);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x15, 0x83);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x16, 0x0c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x29, 0x0a);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x30, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x31, 0xfe);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x32, 0xfd);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x33, 0xfb);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x34, 0xf8);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x35, 0xf5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x36, 0xf3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x37, 0xf2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x38, 0xf2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x39, 0xf2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3a, 0xef);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3b, 0xec);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3d, 0xe9);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3f, 0xe5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x40, 0xe5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x41, 0xe5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x2a, 0x13);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x45, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x46, 0xf4);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x47, 0xe7);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x48, 0xda);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x49, 0xcd);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4a, 0xc0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4b, 0xb3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4c, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4d, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4e, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4f, 0x99);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x50, 0x80);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x51, 0x68);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x52, 0x66);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x53, 0x66);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x54, 0x66);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x2b, 0x0e);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x58, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x59, 0xfb);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5a, 0xf7);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5b, 0xf3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5c, 0xef);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5d, 0xe3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5e, 0xda);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5f, 0xd8);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x60, 0xd8);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x61, 0xd8);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x62, 0xcb);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x63, 0xbf);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x64, 0xb3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x65, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x66, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x67, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x51, 0x0f, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x53, 0x2c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x55, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbb, 0x13);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3b, 0x03, 0xac, 0x1a, 0x04, 0x04);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x2a);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x25, 0x46);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x30, 0x46);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x39, 0x46);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x26);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x01, 0xb0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x19, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1a, 0xe0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1b, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1c, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x2a, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x2b, 0xe0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0xf0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x84, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x85, 0x0c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x51, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x25);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x91, 0x1f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x92, 0x0f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x93, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x94, 0x18);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x95, 0x03);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x96, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb0, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x25);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x19, 0x1f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1b, 0x1b);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x24);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb8, 0x28);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x27);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd0, 0x31);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd1, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd4, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xde, 0x80);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xdf, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x26);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x00, 0x81);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x01, 0xb0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x22);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x6f, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x70, 0x11);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x73, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x74, 0x4d);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xa0, 0x3f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xa9, 0x50);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xaa, 0x28);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xab, 0x28);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xad, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb8, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb9, 0x4b);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xba, 0x96);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbb, 0x4b);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbe, 0x07);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbf, 0x4b);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc0, 0x07);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc1, 0x5c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc2, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc5, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc6, 0x3f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc7, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xca, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xcb, 0x40);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xce, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xcf, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd0, 0x40);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd3, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd4, 0x40);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x25);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbc, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbd, 0x1c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x2a);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x9a, 0x03);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x11);
	msleep(70);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x29);

	return 0;
}

static int nabu_csot_init_sequence(struct panel_info *pinfo)
{
	struct mipi_dsi_device *dsi0 = pinfo->dsi[0];
	struct mipi_dsi_device *dsi1 = pinfo->dsi[1];
	/* No datasheet, so write magic init sequence directly */
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb9, 0x05);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x18, 0x40);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb9, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0xd0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x02, 0xaf);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x00, 0x30);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x09, 0xee);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1c, 0x99);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1d, 0x09);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0xf0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3a, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0xe0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4f, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x35, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x23);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x00, 0x80);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x01, 0x84);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x05, 0x2d);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x06, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x07, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x08, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x09, 0x45);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x11, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x12, 0x80);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x15, 0x83);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x16, 0x0c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x29, 0x0a);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x30, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x31, 0xfe);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x32, 0xfd);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x33, 0xfb);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x34, 0xf8);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x35, 0xf5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x36, 0xf3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x37, 0xf2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x38, 0xf2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x39, 0xf2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3a, 0xef);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3b, 0xec);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3d, 0xe9);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3f, 0xe5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x40, 0xe5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x41, 0xe5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x2a, 0x13);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x45, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x46, 0xf4);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x47, 0xe7);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x48, 0xda);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x49, 0xcd);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4a, 0xc0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4b, 0xb3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4c, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4d, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4e, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x4f, 0x99);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x50, 0x80);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x51, 0x68);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x52, 0x66);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x53, 0x66);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x54, 0x66);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x2b, 0x0e);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x58, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x59, 0xfb);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5a, 0xf7);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5b, 0xf3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5c, 0xef);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5d, 0xe3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5e, 0xda);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5f, 0xd8);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x60, 0xd8);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x61, 0xd8);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x62, 0xcb);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x63, 0xbf);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x64, 0xb3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x65, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x66, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x67, 0xb2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x51, 0x0f, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x53, 0x24);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x55, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbb, 0x13);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3b, 0x03, 0xac, 0x1a, 0x04, 0x04);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x2a);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x25, 0x46);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x30, 0x46);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x39, 0x46);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x26);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x01, 0xb0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x19, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1a, 0xe0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1b, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1c, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x2a, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x2b, 0xe0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x26);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x01, 0xb0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0xf0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x84, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x85, 0x0c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x51, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x25);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x91, 0x1f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x92, 0x0f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x93, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x94, 0x18);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x95, 0x03);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x96, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb0, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x25);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x19, 0x1f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1b, 0x1b);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x24);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb8, 0x28);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x27);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd0, 0x31);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xde, 0x80);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xdf, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd1, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd4, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x26);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x00, 0x81);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x01, 0xb0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x22);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x6f, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x70, 0x11);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x73, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x74, 0x4d);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xa0, 0x3f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xa9, 0x50);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xaa, 0x28);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xab, 0x28);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xad, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb8, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb9, 0x4b);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xba, 0x96);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbb, 0x4b);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbe, 0x07);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbf, 0x4b);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc0, 0x07);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc1, 0x5c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc2, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc5, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc6, 0x3f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xc7, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xca, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xcb, 0x40);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xce, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xcf, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd0, 0x40);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd3, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd4, 0x40);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x25);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbc, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbd, 0x1c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x2a);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x9a, 0x03);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x11);
	msleep(70);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x29);

	return 0;
}

static int kirby_csot_init_sequence(struct panel_info *pinfo)
{
	struct mipi_dsi_device *dsi0 = pinfo->dsi[0];
	struct mipi_dsi_device *dsi1 = pinfo->dsi[1];

	/* Lenovo TB321FU Android qcom,mdss_dsi_kirby_nt36523_csot_dsc_vid */
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf0, 0xaa);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf1, 0x55);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf2, 0x99);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x25);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x0f, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x27);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd0, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd1, 0x54);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xde, 0x03);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xdf, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0xd0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x00, 0x30);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x09, 0xee);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1c, 0x99);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1d, 0x09);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0xf0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x24, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x25);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x17, 0xcf);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x19, 0x1f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1b, 0x5b);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1c, 0xf0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf2, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf3, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x35, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x51, 0x07, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x53, 0x2c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x55, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x90, 0x03);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x91,
				    0x89, 0x28, 0x00, 0x14, 0xc2, 0x00, 0x02,
				    0x91, 0x02, 0x47, 0x00, 0x0b, 0x05, 0x0e,
				    0x03, 0x68);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x92, 0x10, 0xe0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb2, 0x91);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb3, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbb, 0x13);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3b,
				    0x03, 0x04, 0x1e, 0x04, 0x04, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x11);
	msleep(105);

	return 0;
}

static int kirby_csot_display_on_sequence(struct panel_info *pinfo)
{
	struct mipi_dsi_device *dsi0 = pinfo->dsi[0];
	struct mipi_dsi_device *dsi1 = pinfo->dsi[1];

	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x29);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf0, 0x55);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf1, 0xaa);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf2, 0x66);

	return 0;
}

static int lapis_tianma_init_sequence(struct panel_info *pinfo)
{
	struct mipi_dsi_device *dsi0 = pinfo->dsi[0];
	struct mipi_dsi_device *dsi1 = pinfo->dsi[1];

	/* Lenovo TB520FU Android qcom,mdss_dsi_panel_lapis_tianma_nt36532_dsc_3k_video */
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf0, 0xaa);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf1, 0x55);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf2, 0x99);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x0d, 0x43);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x32, 0x72);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x69, 0xdd);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x23);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xba, 0x1f, 0x97);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbb, 0x1d, 0x97);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb1, 0x1c, 0x97);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xbc, 0x0d, 0x00, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x24);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xda, 0x06);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x25);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x0f, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x27);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x13, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x7d, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x7e, 0x0a);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x7f, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd0, 0x71);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd1, 0x84);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd2, 0x38);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xde, 0x43);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xdf, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x2a);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xb1, 0x00, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x23);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x00, 0x60);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x05, 0x24);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x07, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x08, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x09, 0xc2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x10, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x11, 0x03);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x12, 0x41);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x15, 0x64);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x16, 0x15);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x30, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x31, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x32, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x33, 0xfc);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x34, 0xf9);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x35, 0xf5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x36, 0xf2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x37, 0xf0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x38, 0xed);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x39, 0xeb);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3a, 0xe8);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3b, 0xe5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3d, 0xe3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3f, 0xe0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x40, 0xde);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x41, 0xdb);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x58, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x59, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5a, 0xfa);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5b, 0xf7);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5c, 0xf2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5d, 0xeb);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5e, 0xe3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5f, 0xd9);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x60, 0xd1);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x61, 0xcc);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x62, 0xc7);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x63, 0xbf);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x64, 0xba);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x65, 0xb5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x66, 0xb0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x67, 0xab);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x19, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1a, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1b, 0x04);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1c, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1d, 0x0e);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1e, 0x14);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1f, 0x18);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x20, 0x1e);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x21, 0x22);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x22, 0x26);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x23, 0x2a);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x24, 0x30);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x25, 0x34);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x26, 0x38);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x27, 0x3c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x28, 0x3f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x29, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x2b, 0x28);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3b, 0x03, 0xb8, 0x1a, 0x0a, 0x0a, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x51, 0x07, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x53, 0x2c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x55, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x81, 0x02, 0x02, 0x22, 0x00, 0x2a, 0x40, 0x2a, 0xbe, 0x3a, 0xfc, 0x3a, 0xfa, 0x3a, 0xf8, 0x3b, 0x38);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x82, 0x3b, 0x78, 0x3b, 0xb6, 0x4b, 0xb6, 0x4b, 0xf4, 0x4b, 0xf4, 0x6c, 0x34, 0x84, 0x74);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x90, 0x83);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x91, 0xab, 0x28, 0x00, 0x14, 0xf2, 0x00, 0x02, 0xce, 0x01, 0xb0, 0x00, 0x0a, 0x06, 0x51, 0x03, 0xc0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x92, 0x11, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x11);
	msleep(105);

	return 0;
}

static int lapis_boe_init_sequence(struct panel_info *pinfo)
{
	struct mipi_dsi_device *dsi0 = pinfo->dsi[0];
	struct mipi_dsi_device *dsi1 = pinfo->dsi[1];

	/* Lenovo TB520FU Android qcom,mdss_dsi_panel_lapis_boe_nt36532_dsc_3k_video */
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf0, 0xaa);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf1, 0x55);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xf2, 0x99);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x32, 0x72);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3b, 0x03, 0xb8, 0x1a, 0x0a, 0x0a, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x27);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x78, 0x80);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd0, 0x71);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd1, 0x84);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xd2, 0x38);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xde, 0x43);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xdf, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x25);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x0f, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x26);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x13, 0xb5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x23);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x00, 0x60);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x07, 0x20);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x08, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x09, 0xc2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x10, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x11, 0x03);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x12, 0x41);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x15, 0x64);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x16, 0x15);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x30, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x31, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x32, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x33, 0xfc);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x34, 0xf9);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x35, 0xf5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x36, 0xf2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x37, 0xf0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x38, 0xed);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x39, 0xeb);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3a, 0xe8);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3b, 0xe5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3d, 0xe3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x3f, 0xe0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x40, 0xde);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x41, 0xdb);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x58, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x59, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5a, 0xfa);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5b, 0xf7);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5c, 0xf2);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5d, 0xeb);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5e, 0xe3);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x5f, 0xd9);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x60, 0xd1);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x61, 0xcc);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x62, 0xc7);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x63, 0xbf);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x64, 0xba);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x65, 0xb5);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x66, 0xb0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x67, 0xab);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x19, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1a, 0x02);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1b, 0x04);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1c, 0x08);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1d, 0x0e);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1e, 0x14);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x1f, 0x18);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x20, 0x1e);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x21, 0x22);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x22, 0x26);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x23, 0x2a);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x24, 0x30);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x25, 0x34);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x26, 0x38);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x27, 0x3c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x28, 0x3f);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x29, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x2b, 0x28);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xff, 0x10);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0xfb, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x51, 0x07, 0xff);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x53, 0x2c);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x55, 0x01);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x90, 0x03);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x91, 0xab, 0x28, 0x00, 0x14, 0xf2, 0x00, 0x02, 0xce, 0x01, 0xb0, 0x00, 0x0a, 0x06, 0x51, 0x03, 0xc0);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x92, 0x11, 0x00);
	mipi_dsi_dual_dcs_write_seq(dsi0, dsi1, 0x11);
	msleep(105);

	return 0;
}

static int j606f_boe_init_sequence(struct panel_info *pinfo)
{
	struct mipi_dsi_device *dsi = pinfo->dsi[0];
	struct device *dev = &dsi->dev;
	int ret;

	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x20);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x05, 0xd9);
	mipi_dsi_dcs_write_seq(dsi, 0x07, 0x78);
	mipi_dsi_dcs_write_seq(dsi, 0x08, 0x5a);
	mipi_dsi_dcs_write_seq(dsi, 0x0d, 0x63);
	mipi_dsi_dcs_write_seq(dsi, 0x0e, 0x91);
	mipi_dsi_dcs_write_seq(dsi, 0x0f, 0x73);
	mipi_dsi_dcs_write_seq(dsi, 0x95, 0xeb);
	mipi_dsi_dcs_write_seq(dsi, 0x96, 0xeb);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_SET_PARTIAL_ROWS, 0x11);
	mipi_dsi_dcs_write_seq(dsi, 0x6d, 0x66);
	mipi_dsi_dcs_write_seq(dsi, 0x75, 0xa2);
	mipi_dsi_dcs_write_seq(dsi, 0x77, 0xb3);
	mipi_dsi_dcs_write_seq(dsi, 0xb0, 0x00, 0x08, 0x00, 0x23, 0x00, 0x4d, 0x00, 0x6d, 0x00,
			       0x89, 0x00, 0xa1, 0x00, 0xb6, 0x00, 0xc9);
	mipi_dsi_dcs_write_seq(dsi, 0xb1, 0x00, 0xda, 0x01, 0x13, 0x01, 0x3c, 0x01, 0x7e, 0x01,
			       0xab, 0x01, 0xf7, 0x02, 0x2f, 0x02, 0x31);
	mipi_dsi_dcs_write_seq(dsi, 0xb2, 0x02, 0x67, 0x02, 0xa6, 0x02, 0xd1, 0x03, 0x08, 0x03,
			       0x2e, 0x03, 0x5b, 0x03, 0x6b, 0x03, 0x7b);
	mipi_dsi_dcs_write_seq(dsi, 0xb3, 0x03, 0x8e, 0x03, 0xa2, 0x03, 0xb7, 0x03, 0xe7, 0x03,
			       0xfd, 0x03, 0xff);
	mipi_dsi_dcs_write_seq(dsi, 0xb4, 0x00, 0x08, 0x00, 0x23, 0x00, 0x4d, 0x00, 0x6d, 0x00,
			       0x89, 0x00, 0xa1, 0x00, 0xb6, 0x00, 0xc9);
	mipi_dsi_dcs_write_seq(dsi, 0xb5, 0x00, 0xda, 0x01, 0x13, 0x01, 0x3c, 0x01, 0x7e, 0x01,
			       0xab, 0x01, 0xf7, 0x02, 0x2f, 0x02, 0x31);
	mipi_dsi_dcs_write_seq(dsi, 0xb6, 0x02, 0x67, 0x02, 0xa6, 0x02, 0xd1, 0x03, 0x08, 0x03,
			       0x2e, 0x03, 0x5b, 0x03, 0x6b, 0x03, 0x7b);
	mipi_dsi_dcs_write_seq(dsi, 0xb7, 0x03, 0x8e, 0x03, 0xa2, 0x03, 0xb7, 0x03, 0xe7, 0x03,
			       0xfd, 0x03, 0xff);
	mipi_dsi_dcs_write_seq(dsi, 0xb8, 0x00, 0x08, 0x00, 0x23, 0x00, 0x4d, 0x00, 0x6d, 0x00,
			       0x89, 0x00, 0xa1, 0x00, 0xb6, 0x00, 0xc9);
	mipi_dsi_dcs_write_seq(dsi, 0xb9, 0x00, 0xda, 0x01, 0x13, 0x01, 0x3c, 0x01, 0x7e, 0x01,
			       0xab, 0x01, 0xf7, 0x02, 0x2f, 0x02, 0x31);
	mipi_dsi_dcs_write_seq(dsi, 0xba, 0x02, 0x67, 0x02, 0xa6, 0x02, 0xd1, 0x03, 0x08, 0x03,
			       0x2e, 0x03, 0x5b, 0x03, 0x6b, 0x03, 0x7b);
	mipi_dsi_dcs_write_seq(dsi, 0xbb, 0x03, 0x8e, 0x03, 0xa2, 0x03, 0xb7, 0x03, 0xe7, 0x03,
			       0xfd, 0x03, 0xff);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x21);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0xb0, 0x00, 0x00, 0x00, 0x1b, 0x00, 0x45, 0x00, 0x65, 0x00,
			       0x81, 0x00, 0x99, 0x00, 0xae, 0x00, 0xc1);
	mipi_dsi_dcs_write_seq(dsi, 0xb1, 0x00, 0xd2, 0x01, 0x0b, 0x01, 0x34, 0x01, 0x76, 0x01,
			       0xa3, 0x01, 0xef, 0x02, 0x27, 0x02, 0x29);
	mipi_dsi_dcs_write_seq(dsi, 0xb2, 0x02, 0x5f, 0x02, 0x9e, 0x02, 0xc9, 0x03, 0x00, 0x03,
			       0x26, 0x03, 0x53, 0x03, 0x63, 0x03, 0x73);
	mipi_dsi_dcs_write_seq(dsi, 0xb3, 0x03, 0x86, 0x03, 0x9a, 0x03, 0xaf, 0x03, 0xdf, 0x03,
			       0xf5, 0x03, 0xf7);
	mipi_dsi_dcs_write_seq(dsi, 0xb4, 0x00, 0x00, 0x00, 0x1b, 0x00, 0x45, 0x00, 0x65, 0x00,
			       0x81, 0x00, 0x99, 0x00, 0xae, 0x00, 0xc1);
	mipi_dsi_dcs_write_seq(dsi, 0xb5, 0x00, 0xd2, 0x01, 0x0b, 0x01, 0x34, 0x01, 0x76, 0x01,
			       0xa3, 0x01, 0xef, 0x02, 0x27, 0x02, 0x29);
	mipi_dsi_dcs_write_seq(dsi, 0xb6, 0x02, 0x5f, 0x02, 0x9e, 0x02, 0xc9, 0x03, 0x00, 0x03,
			       0x26, 0x03, 0x53, 0x03, 0x63, 0x03, 0x73);
	mipi_dsi_dcs_write_seq(dsi, 0xb7, 0x03, 0x86, 0x03, 0x9a, 0x03, 0xaf, 0x03, 0xdf, 0x03,
			       0xf5, 0x03, 0xf7);
	mipi_dsi_dcs_write_seq(dsi, 0xb8, 0x00, 0x00, 0x00, 0x1b, 0x00, 0x45, 0x00, 0x65, 0x00,
			       0x81, 0x00, 0x99, 0x00, 0xae, 0x00, 0xc1);
	mipi_dsi_dcs_write_seq(dsi, 0xb9, 0x00, 0xd2, 0x01, 0x0b, 0x01, 0x34, 0x01, 0x76, 0x01,
			       0xa3, 0x01, 0xef, 0x02, 0x27, 0x02, 0x29);
	mipi_dsi_dcs_write_seq(dsi, 0xba, 0x02, 0x5f, 0x02, 0x9e, 0x02, 0xc9, 0x03, 0x00, 0x03,
			       0x26, 0x03, 0x53, 0x03, 0x63, 0x03, 0x73);
	mipi_dsi_dcs_write_seq(dsi, 0xbb, 0x03, 0x86, 0x03, 0x9a, 0x03, 0xaf, 0x03, 0xdf, 0x03,
			       0xf5, 0x03, 0xf7);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x23);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x00, 0x80);
	mipi_dsi_dcs_write_seq(dsi, 0x07, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x11, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x12, 0x77);
	mipi_dsi_dcs_write_seq(dsi, 0x15, 0x07);
	mipi_dsi_dcs_write_seq(dsi, 0x16, 0x07);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x24);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x00, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x01, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x02, 0x1c);
	mipi_dsi_dcs_write_seq(dsi, 0x03, 0x1c);
	mipi_dsi_dcs_write_seq(dsi, 0x04, 0x1d);
	mipi_dsi_dcs_write_seq(dsi, 0x05, 0x1d);
	mipi_dsi_dcs_write_seq(dsi, 0x06, 0x04);
	mipi_dsi_dcs_write_seq(dsi, 0x07, 0x04);
	mipi_dsi_dcs_write_seq(dsi, 0x08, 0x0f);
	mipi_dsi_dcs_write_seq(dsi, 0x09, 0x0f);
	mipi_dsi_dcs_write_seq(dsi, 0x0a, 0x0e);
	mipi_dsi_dcs_write_seq(dsi, 0x0b, 0x0e);
	mipi_dsi_dcs_write_seq(dsi, 0x0c, 0x0d);
	mipi_dsi_dcs_write_seq(dsi, 0x0d, 0x0d);
	mipi_dsi_dcs_write_seq(dsi, 0x0e, 0x0c);
	mipi_dsi_dcs_write_seq(dsi, 0x0f, 0x0c);
	mipi_dsi_dcs_write_seq(dsi, 0x10, 0x08);
	mipi_dsi_dcs_write_seq(dsi, 0x11, 0x08);
	mipi_dsi_dcs_write_seq(dsi, 0x12, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x13, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x14, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x15, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x16, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x17, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x18, 0x1c);
	mipi_dsi_dcs_write_seq(dsi, 0x19, 0x1c);
	mipi_dsi_dcs_write_seq(dsi, 0x1a, 0x1d);
	mipi_dsi_dcs_write_seq(dsi, 0x1b, 0x1d);
	mipi_dsi_dcs_write_seq(dsi, 0x1c, 0x04);
	mipi_dsi_dcs_write_seq(dsi, 0x1d, 0x04);
	mipi_dsi_dcs_write_seq(dsi, 0x1e, 0x0f);
	mipi_dsi_dcs_write_seq(dsi, 0x1f, 0x0f);
	mipi_dsi_dcs_write_seq(dsi, 0x20, 0x0e);
	mipi_dsi_dcs_write_seq(dsi, 0x21, 0x0e);
	mipi_dsi_dcs_write_seq(dsi, 0x22, 0x0d);
	mipi_dsi_dcs_write_seq(dsi, 0x23, 0x0d);
	mipi_dsi_dcs_write_seq(dsi, 0x24, 0x0c);
	mipi_dsi_dcs_write_seq(dsi, 0x25, 0x0c);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_SET_GAMMA_CURVE, 0x08);
	mipi_dsi_dcs_write_seq(dsi, 0x27, 0x08);
	mipi_dsi_dcs_write_seq(dsi, 0x28, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x29, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x2a, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x2b, 0x00);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_WRITE_LUT, 0x20);
	mipi_dsi_dcs_write_seq(dsi, 0x2f, 0x0a);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_SET_PARTIAL_ROWS, 0x44);
	mipi_dsi_dcs_write_seq(dsi, 0x33, 0x0c);
	mipi_dsi_dcs_write_seq(dsi, 0x34, 0x32);
	mipi_dsi_dcs_write_seq(dsi, 0x37, 0x44);
	mipi_dsi_dcs_write_seq(dsi, 0x38, 0x40);
	mipi_dsi_dcs_write_seq(dsi, 0x39, 0x00);

	ret = mipi_dsi_dcs_set_pixel_format(dsi, 0x9a);
	if (ret < 0) {
		dev_err(dev, "Failed to set pixel format: %d\n", ret);
		return ret;
	}

	mipi_dsi_dcs_write_seq(dsi, 0x3b, 0xa0);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_SET_3D_CONTROL, 0x42);
	mipi_dsi_dcs_write_seq(dsi, 0x3f, 0x06);
	mipi_dsi_dcs_write_seq(dsi, 0x43, 0x06);
	mipi_dsi_dcs_write_seq(dsi, 0x47, 0x66);
	mipi_dsi_dcs_write_seq(dsi, 0x4a, 0x9a);
	mipi_dsi_dcs_write_seq(dsi, 0x4b, 0xa0);
	mipi_dsi_dcs_write_seq(dsi, 0x4c, 0x91);
	mipi_dsi_dcs_write_seq(dsi, 0x4d, 0x21);
	mipi_dsi_dcs_write_seq(dsi, 0x4e, 0x43);

	ret = mipi_dsi_dcs_set_display_brightness(dsi, 18);
	if (ret < 0) {
		dev_err(dev, "Failed to set display brightness: %d\n", ret);
		return ret;
	}

	mipi_dsi_dcs_write_seq(dsi, 0x52, 0x34);
	mipi_dsi_dcs_write_seq(dsi, 0x55, 0x82, 0x02);
	mipi_dsi_dcs_write_seq(dsi, 0x56, 0x04);
	mipi_dsi_dcs_write_seq(dsi, 0x58, 0x21);
	mipi_dsi_dcs_write_seq(dsi, 0x59, 0x30);
	mipi_dsi_dcs_write_seq(dsi, 0x5a, 0xba);
	mipi_dsi_dcs_write_seq(dsi, 0x5b, 0xa0);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_SET_CABC_MIN_BRIGHTNESS, 0x00, 0x06);
	mipi_dsi_dcs_write_seq(dsi, 0x5f, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x65, 0x82);
	mipi_dsi_dcs_write_seq(dsi, 0x7e, 0x20);
	mipi_dsi_dcs_write_seq(dsi, 0x7f, 0x3c);
	mipi_dsi_dcs_write_seq(dsi, 0x82, 0x04);
	mipi_dsi_dcs_write_seq(dsi, 0x97, 0xc0);
	mipi_dsi_dcs_write_seq(dsi, 0xb6,
			       0x05, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05,
			       0x05, 0x00, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x92, 0xc4);
	mipi_dsi_dcs_write_seq(dsi, 0x93, 0x1a);
	mipi_dsi_dcs_write_seq(dsi, 0x94, 0x5f);
	mipi_dsi_dcs_write_seq(dsi, 0xd7, 0x55);
	mipi_dsi_dcs_write_seq(dsi, 0xda, 0x0a);
	mipi_dsi_dcs_write_seq(dsi, 0xde, 0x08);
	mipi_dsi_dcs_write_seq(dsi, 0xdb, 0x05);
	mipi_dsi_dcs_write_seq(dsi, 0xdc, 0xc4);
	mipi_dsi_dcs_write_seq(dsi, 0xdd, 0x22);
	mipi_dsi_dcs_write_seq(dsi, 0xdf, 0x05);
	mipi_dsi_dcs_write_seq(dsi, 0xe0, 0xc4);
	mipi_dsi_dcs_write_seq(dsi, 0xe1, 0x05);
	mipi_dsi_dcs_write_seq(dsi, 0xe2, 0xc4);
	mipi_dsi_dcs_write_seq(dsi, 0xe3, 0x05);
	mipi_dsi_dcs_write_seq(dsi, 0xe4, 0xc4);
	mipi_dsi_dcs_write_seq(dsi, 0xe5, 0x05);
	mipi_dsi_dcs_write_seq(dsi, 0xe6, 0xc4);
	mipi_dsi_dcs_write_seq(dsi, 0x5c, 0x88);
	mipi_dsi_dcs_write_seq(dsi, 0x5d, 0x08);
	mipi_dsi_dcs_write_seq(dsi, 0x8d, 0x88);
	mipi_dsi_dcs_write_seq(dsi, 0x8e, 0x08);
	mipi_dsi_dcs_write_seq(dsi, 0xb5, 0x90);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x25);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x05, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x19, 0x07);
	mipi_dsi_dcs_write_seq(dsi, 0x1f, 0xba);
	mipi_dsi_dcs_write_seq(dsi, 0x20, 0xa0);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_SET_GAMMA_CURVE, 0xba);
	mipi_dsi_dcs_write_seq(dsi, 0x27, 0xa0);
	mipi_dsi_dcs_write_seq(dsi, 0x33, 0xba);
	mipi_dsi_dcs_write_seq(dsi, 0x34, 0xa0);
	mipi_dsi_dcs_write_seq(dsi, 0x3f, 0xe0);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_SET_VSYNC_TIMING, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x44, 0x00);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_GET_SCANLINE, 0x40);
	mipi_dsi_dcs_write_seq(dsi, 0x48, 0xba);
	mipi_dsi_dcs_write_seq(dsi, 0x49, 0xa0);
	mipi_dsi_dcs_write_seq(dsi, 0x5b, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x5c, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x5d, 0x00);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_SET_CABC_MIN_BRIGHTNESS, 0xd0);
	mipi_dsi_dcs_write_seq(dsi, 0x61, 0xba);
	mipi_dsi_dcs_write_seq(dsi, 0x62, 0xa0);
	mipi_dsi_dcs_write_seq(dsi, 0xf1, 0x10);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x2a);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x64, 0x16);
	mipi_dsi_dcs_write_seq(dsi, 0x67, 0x16);
	mipi_dsi_dcs_write_seq(dsi, 0x6a, 0x16);
	mipi_dsi_dcs_write_seq(dsi, 0x70, 0x30);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_READ_PPS_START, 0xf3);
	mipi_dsi_dcs_write_seq(dsi, 0xa3, 0xff);
	mipi_dsi_dcs_write_seq(dsi, 0xa4, 0xff);
	mipi_dsi_dcs_write_seq(dsi, 0xa5, 0xff);
	mipi_dsi_dcs_write_seq(dsi, 0xd6, 0x08);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x26);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x00, 0xa1);
	mipi_dsi_dcs_write_seq(dsi, 0x0a, 0xf2);
	mipi_dsi_dcs_write_seq(dsi, 0x04, 0x28);
	mipi_dsi_dcs_write_seq(dsi, 0x06, 0x30);
	mipi_dsi_dcs_write_seq(dsi, 0x0c, 0x13);
	mipi_dsi_dcs_write_seq(dsi, 0x0d, 0x0a);
	mipi_dsi_dcs_write_seq(dsi, 0x0f, 0x0a);
	mipi_dsi_dcs_write_seq(dsi, 0x11, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x12, 0x50);
	mipi_dsi_dcs_write_seq(dsi, 0x13, 0x51);
	mipi_dsi_dcs_write_seq(dsi, 0x14, 0x65);
	mipi_dsi_dcs_write_seq(dsi, 0x15, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x16, 0x10);
	mipi_dsi_dcs_write_seq(dsi, 0x17, 0xa0);
	mipi_dsi_dcs_write_seq(dsi, 0x18, 0x86);
	mipi_dsi_dcs_write_seq(dsi, 0x19, 0x11);
	mipi_dsi_dcs_write_seq(dsi, 0x1a, 0x7b);
	mipi_dsi_dcs_write_seq(dsi, 0x1b, 0x10);
	mipi_dsi_dcs_write_seq(dsi, 0x1c, 0xbb);
	mipi_dsi_dcs_write_seq(dsi, 0x22, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x23, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x2a, 0x11);
	mipi_dsi_dcs_write_seq(dsi, 0x2b, 0x7b);
	mipi_dsi_dcs_write_seq(dsi, 0x1d, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x1e, 0xc3);
	mipi_dsi_dcs_write_seq(dsi, 0x1f, 0xc3);
	mipi_dsi_dcs_write_seq(dsi, 0x24, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x25, 0xc3);
	mipi_dsi_dcs_write_seq(dsi, 0x2f, 0x05);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_SET_PARTIAL_ROWS, 0xc3);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_SET_PARTIAL_COLUMNS, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x32, 0xc3);
	mipi_dsi_dcs_write_seq(dsi, 0x39, 0x00);

	ret = mipi_dsi_dcs_set_pixel_format(dsi, 0xc3);
	if (ret < 0) {
		dev_err(dev, "Failed to set pixel format: %d\n", ret);
		return ret;
	}

	mipi_dsi_dcs_write_seq(dsi, 0x20, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x33, 0x11);
	mipi_dsi_dcs_write_seq(dsi, 0x34, 0x78);
	mipi_dsi_dcs_write_seq(dsi, 0x35, 0x16);
	mipi_dsi_dcs_write_seq(dsi, 0xc8, 0x04);
	mipi_dsi_dcs_write_seq(dsi, 0xc9, 0x82);
	mipi_dsi_dcs_write_seq(dsi, 0xca, 0x4e);
	mipi_dsi_dcs_write_seq(dsi, 0xcb, 0x00);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_READ_PPS_CONTINUE, 0x4c);
	mipi_dsi_dcs_write_seq(dsi, 0xaa, 0x47);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x27);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x56, 0x06);
	mipi_dsi_dcs_write_seq(dsi, 0x58, 0x80);
	mipi_dsi_dcs_write_seq(dsi, 0x59, 0x53);
	mipi_dsi_dcs_write_seq(dsi, 0x5a, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x5b, 0x14);
	mipi_dsi_dcs_write_seq(dsi, 0x5c, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x5d, 0x01);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_SET_CABC_MIN_BRIGHTNESS, 0x20);
	mipi_dsi_dcs_write_seq(dsi, 0x5f, 0x10);
	mipi_dsi_dcs_write_seq(dsi, 0x60, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x61, 0x1d);
	mipi_dsi_dcs_write_seq(dsi, 0x62, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x63, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x64, 0x24);
	mipi_dsi_dcs_write_seq(dsi, 0x65, 0x1c);
	mipi_dsi_dcs_write_seq(dsi, 0x66, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x67, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x68, 0x25);
	mipi_dsi_dcs_write_seq(dsi, 0x00, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x78, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0xc3, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0xd1, 0x24);
	mipi_dsi_dcs_write_seq(dsi, 0xd2, 0x30);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x2a);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x22, 0x2f);
	mipi_dsi_dcs_write_seq(dsi, 0x23, 0x08);
	mipi_dsi_dcs_write_seq(dsi, 0x24, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x25, 0xc3);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_SET_GAMMA_CURVE, 0xf8);
	mipi_dsi_dcs_write_seq(dsi, 0x27, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x28, 0x1a);
	mipi_dsi_dcs_write_seq(dsi, 0x29, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x2a, 0x1a);
	mipi_dsi_dcs_write_seq(dsi, 0x2b, 0x00);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_WRITE_LUT, 0x1a);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0xe0);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x14, 0x60);
	mipi_dsi_dcs_write_seq(dsi, 0x16, 0xc0);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0xf0);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);

	ret = mipi_dsi_dcs_set_pixel_format(dsi, 0x08);
	if (ret < 0) {
		dev_err(dev, "Failed to set pixel format: %d\n", ret);
		return ret;
	}

	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x24);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);

	ret = mipi_dsi_dcs_set_pixel_format(dsi, 0x5d);
	if (ret < 0) {
		dev_err(dev, "Failed to set pixel format: %d\n", ret);
		return ret;
	}

	mipi_dsi_dcs_write_seq(dsi, 0x3b, 0x60);
	mipi_dsi_dcs_write_seq(dsi, 0x4a, 0x5d);
	mipi_dsi_dcs_write_seq(dsi, 0x4b, 0x60);
	mipi_dsi_dcs_write_seq(dsi, 0x5a, 0x70);
	mipi_dsi_dcs_write_seq(dsi, 0x5b, 0x60);
	mipi_dsi_dcs_write_seq(dsi, 0x91, 0x44);
	mipi_dsi_dcs_write_seq(dsi, 0x92, 0x75);
	mipi_dsi_dcs_write_seq(dsi, 0xdb, 0x05);
	mipi_dsi_dcs_write_seq(dsi, 0xdc, 0x75);
	mipi_dsi_dcs_write_seq(dsi, 0xdd, 0x22);
	mipi_dsi_dcs_write_seq(dsi, 0xdf, 0x05);
	mipi_dsi_dcs_write_seq(dsi, 0xe0, 0x75);
	mipi_dsi_dcs_write_seq(dsi, 0xe1, 0x05);
	mipi_dsi_dcs_write_seq(dsi, 0xe2, 0x75);
	mipi_dsi_dcs_write_seq(dsi, 0xe3, 0x05);
	mipi_dsi_dcs_write_seq(dsi, 0xe4, 0x75);
	mipi_dsi_dcs_write_seq(dsi, 0xe5, 0x05);
	mipi_dsi_dcs_write_seq(dsi, 0xe6, 0x75);
	mipi_dsi_dcs_write_seq(dsi, 0x5c, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x5d, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x8d, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x8e, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x25);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x1f, 0x70);
	mipi_dsi_dcs_write_seq(dsi, 0x20, 0x60);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_SET_GAMMA_CURVE, 0x70);
	mipi_dsi_dcs_write_seq(dsi, 0x27, 0x60);
	mipi_dsi_dcs_write_seq(dsi, 0x33, 0x70);
	mipi_dsi_dcs_write_seq(dsi, 0x34, 0x60);
	mipi_dsi_dcs_write_seq(dsi, 0x48, 0x70);
	mipi_dsi_dcs_write_seq(dsi, 0x49, 0x60);
	mipi_dsi_dcs_write_seq(dsi, 0x5b, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x61, 0x70);
	mipi_dsi_dcs_write_seq(dsi, 0x62, 0x60);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x26);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x02, 0x31);
	mipi_dsi_dcs_write_seq(dsi, 0x19, 0x0a);
	mipi_dsi_dcs_write_seq(dsi, 0x1a, 0x7f);
	mipi_dsi_dcs_write_seq(dsi, 0x1b, 0x0a);
	mipi_dsi_dcs_write_seq(dsi, 0x1c, 0x0c);
	mipi_dsi_dcs_write_seq(dsi, 0x2a, 0x0a);
	mipi_dsi_dcs_write_seq(dsi, 0x2b, 0x7f);
	mipi_dsi_dcs_write_seq(dsi, 0x1e, 0x75);
	mipi_dsi_dcs_write_seq(dsi, 0x1f, 0x75);
	mipi_dsi_dcs_write_seq(dsi, 0x25, 0x75);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_SET_PARTIAL_ROWS, 0x75);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_SET_PARTIAL_COLUMNS, 0x05);
	mipi_dsi_dcs_write_seq(dsi, 0x32, 0x8d);

	ret = mipi_dsi_dcs_set_pixel_format(dsi, 0x75);
	if (ret < 0) {
		dev_err(dev, "Failed to set pixel format: %d\n", ret);
		return ret;
	}

	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x2a);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x25, 0x75);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x10);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0xb9, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x20);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0x18, 0x40);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x10);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);
	mipi_dsi_dcs_write_seq(dsi, 0xb9, 0x02);

	ret = mipi_dsi_dcs_set_tear_on(dsi, MIPI_DSI_DCS_TEAR_MODE_VBLANK);
	if (ret < 0) {
		dev_err(dev, "Failed to set tear on: %d\n", ret);
		return ret;
	}

	mipi_dsi_dcs_write_seq(dsi, 0xbb, 0x13);
	mipi_dsi_dcs_write_seq(dsi, 0x3b, 0x03, 0x5f, 0x1a, 0x04, 0x04);
	mipi_dsi_dcs_write_seq(dsi, 0xff, 0x10);
	usleep_range(10000, 11000);
	mipi_dsi_dcs_write_seq(dsi, 0xfb, 0x01);

	ret = mipi_dsi_dcs_set_display_brightness(dsi, 0);
	if (ret < 0) {
		dev_err(dev, "Failed to set display brightness: %d\n", ret);
		return ret;
	}

	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_WRITE_CONTROL_DISPLAY, 0x2c);
	mipi_dsi_dcs_write_seq(dsi, MIPI_DCS_WRITE_POWER_SAVE, 0x00);
	mipi_dsi_dcs_write_seq(dsi, 0x68, 0x05, 0x01);

	ret = mipi_dsi_dcs_exit_sleep_mode(dsi);
	if (ret < 0) {
		dev_err(dev, "Failed to exit sleep mode: %d\n", ret);
		return ret;
	}
	msleep(100);

	ret = mipi_dsi_dcs_set_display_on(dsi);
	if (ret < 0) {
		dev_err(dev, "Failed to set display on: %d\n", ret);
		return ret;
	}
	msleep(30);

	return 0;
}

static const struct drm_display_mode elish_boe_modes[] = {
	{
		.clock = (1600 + 60 + 8 + 60) * (2560 + 26 + 4 + 168) * 120 / 1000,
		.hdisplay = 1600,
		.hsync_start = 1600 + 60,
		.hsync_end = 1600 + 60 + 8,
		.htotal = 1600 + 60 + 8 + 60,
		.vdisplay = 2560,
		.vsync_start = 2560 + 26,
		.vsync_end = 2560 + 26 + 4,
		.vtotal = 2560 + 26 + 4 + 168,
	},
};

static const struct drm_display_mode elish_csot_modes[] = {
	{
		.clock = (1600 + 200 + 40 + 52) * (2560 + 26 + 4 + 168) * 120 / 1000,
		.hdisplay = 1600,
		.hsync_start = 1600 + 200,
		.hsync_end = 1600 + 200 + 40,
		.htotal = 1600 + 200 + 40 + 52,
		.vdisplay = 2560,
		.vsync_start = 2560 + 26,
		.vsync_end = 2560 + 26 + 4,
		.vtotal = 2560 + 26 + 4 + 168,
	},
};

static const struct drm_display_mode nabu_csot_modes[] = {
	{
		.clock = (1600 + 88 + 40 + 40) * (2560 + 26 + 4 + 168) * 120 / 1000,
		.hdisplay = 1600,
		.hsync_start = 1600 + 88,
		.hsync_end = 1600 + 88 + 40,
		.htotal = 1600 + 88 + 40 + 40,
		.vdisplay = 2560,
		.vsync_start = 2560 + 26,
		.vsync_end = 2560 + 26 + 4,
		.vtotal = 2560 + 26 + 4 + 168,
	},
};

static const struct drm_display_mode kirby_csot_modes[] = {
	{
		/*
		 * Daily baseline mode. This matches the Android panel command
		 * class using B2=0x91/B3=0x01 and keeps the validated bonded DSI,
		 * DSC, and porch tuple.
		 */
		.clock = 791388,
		.hdisplay = 1600,
		.hsync_start = 1600 + 566,
		.hsync_end = 1600 + 566 + 40,
		.htotal = 1600 + 566 + 40 + 108,
		.vdisplay = 2560,
		.vsync_start = 2560 + 30,
		.vsync_end = 2560 + 30 + 4,
		.vtotal = 2560 + 30 + 4 + 256,
	},
	{
		/*
		 * Android dynamic-FPS porch mode keeps the 120 Hz link clock for
		 * this refresh class and lowers the frame rate by extending VFP.
		 * Other refresh classes need explicit panel switch commands.
		 */
		.clock = 791388,
		.hdisplay = 1600,
		.hsync_start = 1600 + 566,
		.hsync_end = 1600 + 566 + 40,
		.htotal = 1600 + 566 + 40 + 108,
		.vdisplay = 2560,
		.vsync_start = 2560 + 2880,
		.vsync_end = 2560 + 2880 + 4,
		.vtotal = 2560 + 2880 + 4 + 256,
	},
};

static const struct drm_display_mode lapis_nt36532_modes[] = {
	{
		/*
		 * Lenovo TB520FU (Lapis) 2944x1840 @ 120Hz Bonded Dual-DSI DSC mode.
		 * Per-link: hdisplay=1472, hfp=139, hpw=16, hbp=64;
		 * vdisplay=1840, vfp=26, vpw=2, vbp=182.
		 */
		.clock = 831972,
		.hdisplay = 2944,
		.hsync_start = 2944 + 278,
		.hsync_end = 2944 + 278 + 32,
		.htotal = 2944 + 278 + 32 + 128,
		.vdisplay = 1840,
		.vsync_start = 1840 + 26,
		.vsync_end = 1840 + 26 + 2,
		.vtotal = 1840 + 26 + 2 + 182,
	},
	{
		/*
		 * Lenovo TB520FU (Lapis) 2944x1840 @ 60Hz dynamic-FPS porch mode
		 * (keeps 120Hz link clock, extends VFP from 26 to 2076).
		 */
		.clock = 831972,
		.hdisplay = 2944,
		.hsync_start = 2944 + 278,
		.hsync_end = 2944 + 278 + 32,
		.htotal = 2944 + 278 + 32 + 128,
		.vdisplay = 1840,
		.vsync_start = 1840 + 2076,
		.vsync_end = 1840 + 2076 + 2,
		.vtotal = 1840 + 2076 + 2 + 182,
	},
};

static const struct drm_display_mode j606f_boe_modes[] = {
	{
		.clock = (1200 + 58 + 2 + 60) * (2000 + 26 + 2 + 93) * 60 / 1000,
		.hdisplay = 1200,
		.hsync_start = 1200 + 58,
		.hsync_end = 1200 + 58 + 2,
		.htotal = 1200 + 58 + 2 + 60,
		.vdisplay = 2000,
		.vsync_start = 2000 + 26,
		.vsync_end = 2000 + 26 + 2,
		.vtotal = 2000 + 26 + 2 + 93,
		.width_mm = 143,
		.height_mm = 235,
	},
};

static const struct panel_desc elish_boe_desc = {
	.modes = elish_boe_modes,
	.num_modes = ARRAY_SIZE(elish_boe_modes),
	.dsi_info = {
		.type = "BOE-elish",
		.channel = 0,
		.node = NULL,
	},
	.width_mm = 127,
	.height_mm = 203,
	.bpc = 8,
	.lanes = 3,
	.format = MIPI_DSI_FMT_RGB888,
	.mode_flags = MIPI_DSI_MODE_VIDEO | MIPI_DSI_CLOCK_NON_CONTINUOUS | MIPI_DSI_MODE_LPM,
	.init_sequence = elish_boe_init_sequence,
	.is_dual_dsi = true,
};

static const struct panel_desc elish_csot_desc = {
	.modes = elish_csot_modes,
	.num_modes = ARRAY_SIZE(elish_csot_modes),
	.dsi_info = {
		.type = "CSOT-elish",
		.channel = 0,
		.node = NULL,
	},
	.width_mm = 127,
	.height_mm = 203,
	.bpc = 8,
	.lanes = 3,
	.format = MIPI_DSI_FMT_RGB888,
	.mode_flags = MIPI_DSI_MODE_VIDEO | MIPI_DSI_CLOCK_NON_CONTINUOUS | MIPI_DSI_MODE_LPM,
	.init_sequence = elish_csot_init_sequence,
	.is_dual_dsi = true,
};

static const struct panel_desc nabu_csot_desc = {
	.modes = nabu_csot_modes,
	.num_modes = ARRAY_SIZE(nabu_csot_modes),
	.dsi_info = {
		.type = "CSOT-nabu",
		.channel = 0,
		.node = NULL,
	},
	.width_mm = 148,
	.height_mm = 236,
	.bpc = 8,
	.lanes = 3,
	.format = MIPI_DSI_FMT_RGB888,
	.mode_flags = MIPI_DSI_MODE_VIDEO | MIPI_DSI_CLOCK_NON_CONTINUOUS | MIPI_DSI_MODE_LPM,
	.init_sequence = nabu_csot_init_sequence,
	.is_dual_dsi = true,
};

static const struct panel_desc kirby_csot_desc = {
	.modes = kirby_csot_modes,
	.num_modes = ARRAY_SIZE(kirby_csot_modes),
	.dsi_info = {
		.type = "nt36523-kirby",
		.channel = 0,
		.node = NULL,
	},
	.width_mm = 118,
	.height_mm = 190,
	.bpc = 8,
	.lanes = 3,
	.format = MIPI_DSI_FMT_RGB888,
	.mode_flags = MIPI_DSI_MODE_VIDEO | MIPI_DSI_MODE_VIDEO_BURST |
		      MIPI_DSI_MODE_NO_EOT_PACKET |
		      MIPI_DSI_CLOCK_NON_CONTINUOUS | MIPI_DSI_MODE_LPM,
	.init_sequence = kirby_csot_init_sequence,
	.is_dual_dsi = true,
	.has_dsc = true,
	.display_on_after_dsc = true,
	.dsc_pic_width = 1600,
	.dsc_pic_height = 2560,
	.dsc_slice_width = 800,
	.dsc_slice_height = 20,
	.dsc_slice_per_pkt = 1,
};

static const struct panel_desc lapis_tianma_desc = {
	.modes = lapis_nt36532_modes,
	.num_modes = ARRAY_SIZE(lapis_nt36532_modes),
	.dsi_info = {
		.type = "nt36532-lapis-tm",
		.channel = 0,
		.node = NULL,
	},
	.width_mm = 274,
	.height_mm = 171,
	.bpc = 10,
	.lanes = 4,
	/* Android: qcom,mdss-dsi-bpp = <30>, qcom,mdss-dsc-slice-per-pkt = <2> */
	.format = MIPI_DSI_FMT_RGB101010,
	.mode_flags = MIPI_DSI_MODE_VIDEO | MIPI_DSI_MODE_VIDEO_BURST |
		      MIPI_DSI_CLOCK_NON_CONTINUOUS | MIPI_DSI_MODE_LPM |
		      MIPI_DSI_MODE_DSC_ALL_SLICES_IN_PKT,
	.init_sequence = lapis_tianma_init_sequence,
	.is_dual_dsi = true,
	.has_dcs_backlight = false,
	.has_dsc = true,
	.display_on_after_dsc = true,
	.dsc_pic_width = 2944,
	.dsc_pic_height = 1840,
	.dsc_slice_width = 736,
	.dsc_slice_height = 20,
	.dsc_slice_per_pkt = 2,
	.dsc_first_line_bpg_offset = 15,
};

static const struct panel_desc lapis_boe_desc = {
	.modes = lapis_nt36532_modes,
	.num_modes = ARRAY_SIZE(lapis_nt36532_modes),
	.dsi_info = {
		.type = "nt36532-lapis-boe",
		.channel = 0,
		.node = NULL,
	},
	.width_mm = 274,
	.height_mm = 171,
	.bpc = 10,
	.lanes = 4,
	/* Android: qcom,mdss-dsi-bpp = <30>, qcom,mdss-dsc-slice-per-pkt = <2> */
	.format = MIPI_DSI_FMT_RGB101010,
	.mode_flags = MIPI_DSI_MODE_VIDEO | MIPI_DSI_MODE_VIDEO_BURST |
		      MIPI_DSI_CLOCK_NON_CONTINUOUS | MIPI_DSI_MODE_LPM |
		      MIPI_DSI_MODE_DSC_ALL_SLICES_IN_PKT,
	.init_sequence = lapis_boe_init_sequence,
	.is_dual_dsi = true,
	.has_dcs_backlight = false,
	.has_dsc = true,
	.display_on_after_dsc = true,
	.dsc_pic_width = 2944,
	.dsc_pic_height = 1840,
	.dsc_slice_width = 736,
	.dsc_slice_height = 20,
	.dsc_slice_per_pkt = 2,
	.dsc_first_line_bpg_offset = 15,
};

static const struct panel_desc j606f_boe_desc = {
	.modes = j606f_boe_modes,
	.num_modes = ARRAY_SIZE(j606f_boe_modes),
	.width_mm = 143,
	.height_mm = 235,
	.bpc = 8,
	.lanes = 4,
	.format = MIPI_DSI_FMT_RGB888,
	.mode_flags = MIPI_DSI_MODE_VIDEO | MIPI_DSI_MODE_VIDEO_BURST |
		      MIPI_DSI_CLOCK_NON_CONTINUOUS | MIPI_DSI_MODE_LPM,
	.init_sequence = j606f_boe_init_sequence,
	.has_dcs_backlight = true,
};

/* TB520FU bring-up knobs: apply on the next blank/unblank of fb0. */
static bool dbg_pps_full_width = true;
module_param_named(pps_full_width, dbg_pps_full_width, bool, 0644);
static bool dbg_pps_after_display_on = true;
module_param_named(pps_after_display_on, dbg_pps_after_display_on, bool, 0644);
static bool dbg_pps_before_init;
module_param_named(pps_before_init, dbg_pps_before_init, bool, 0644);
static bool dbg_skip_compression_mode;
module_param_named(skip_compression_mode, dbg_skip_compression_mode, bool, 0644);
/* dcs parameter: -1 both links, or one link; dcs_hs clears LPM for the transfer. */
static int dbg_dcs_link = -1;
module_param_named(dcs_link, dbg_dcs_link, int, 0644);
static bool dbg_dcs_hs;
module_param_named(dcs_hs, dbg_dcs_hs, bool, 0644);
static bool dbg_read_hs = true;
module_param_named(read_hs, dbg_read_hs, bool, 0644);
static bool dbg_read_status = true;
module_param_named(read_status, dbg_read_status, bool, 0644);
static bool dbg_pps_minor2;
module_param_named(pps_minor2, dbg_pps_minor2, bool, 0644);

static struct panel_info *dbg_pinfo;

/* "r <reg> <len>" reads, "w <byte>..." writes; both links, hex, see dmesg. */
static int dbg_dcs_set(const char *val, const struct kernel_param *kp)
{
	struct panel_info *pinfo = dbg_pinfo;
	char *args, *cur, *tok;
	u8 buf[64];
	int n = 0, i, ret, op;
	unsigned long saved;

	if (!pinfo)
		return -ENODEV;

	args = kstrdup(val, GFP_KERNEL);
	if (!args)
		return -ENOMEM;

	cur = strim(args);
	tok = strsep(&cur, " ");
	op = tok ? tok[0] : 0;
	while ((tok = strsep(&cur, " ")) && n < ARRAY_SIZE(buf))
		if (*tok && kstrtou8(tok, 16, &buf[n++])) {
			kfree(args);
			return -EINVAL;
		}
	kfree(args);

	for (i = 0; i < DSI_NUM_MIN + pinfo->desc->is_dual_dsi; i++) {
		if (dbg_dcs_link >= 0 && i != dbg_dcs_link)
			continue;

		saved = pinfo->dsi[i]->mode_flags;
		if (dbg_dcs_hs)
			pinfo->dsi[i]->mode_flags &= ~MIPI_DSI_MODE_LPM;

		if (op == 'r' && n == 2) {
			u8 reg = buf[0], len = min_t(u8, buf[1], 32), out[32] = {};

			ret = mipi_dsi_dcs_read(pinfo->dsi[i], reg, out, len);
			dev_info(pinfo->panel.dev, "dcs dsi%d read %02x ret=%d: %*ph\n",
				 i, reg, ret, len, out);
		} else if (op == 'w' && n) {
			ret = mipi_dsi_dcs_write_buffer(pinfo->dsi[i], buf, n);
			dev_info(pinfo->panel.dev, "dcs dsi%d write %*ph ret=%d\n",
				 i, n, buf, ret);
		} else {
			pinfo->dsi[i]->mode_flags = saved;
			return -EINVAL;
		}
		pinfo->dsi[i]->mode_flags = saved;
	}

	return 0;
}

static const struct kernel_param_ops dbg_dcs_ops = { .set = dbg_dcs_set };
module_param_cb(dcs, &dbg_dcs_ops, NULL, 0200);

static int nt36523_enable_dsc(struct panel_info *pinfo)
{
	struct drm_dsc_config pps_dsc = pinfo->dsc;
	struct drm_dsc_picture_parameter_set pps;
	struct device *dev = pinfo->panel.dev;
	unsigned int slices_per_link = pinfo->desc->dsc_slice_per_pkt ? : 1;
	int i, ret;

	/*
	 * Keep the MSM host-side DSC config as the full bonded panel width,
	 * but send each physical DSI sink the per-link PPS that matches the
	 * downstream per-controller qcom,mdss-dsi-panel-width view.
	 */
	if (dbg_pps_full_width) {
		pps_dsc.pic_width = pinfo->desc->dsc_pic_width;
		pps_dsc.slice_count = pinfo->desc->dsc_pic_width / pps_dsc.slice_width;
	} else {
		pps_dsc.pic_width = pps_dsc.slice_width * slices_per_link;
		pps_dsc.slice_count = slices_per_link;
	}

	if (dbg_pps_minor2)
		pps_dsc.dsc_version_minor = 2;

	dev_info(dev, "PPS %ux%u slice %ux%u fl=%u nfl=%u slice_bpg=%u idd=%u sinc=%u sdec=%u\n",
		 pps_dsc.pic_width, pps_dsc.pic_height, pps_dsc.slice_width,
		 pps_dsc.slice_height, pps_dsc.first_line_bpg_offset,
		 pps_dsc.nfl_bpg_offset, pps_dsc.slice_bpg_offset,
		 pps_dsc.initial_dec_delay, pps_dsc.scale_increment_interval,
		 pps_dsc.scale_decrement_interval);

	drm_dsc_pps_payload_pack(&pps, &pps_dsc);

	for (i = 0; i < DSI_NUM_MIN + pinfo->desc->is_dual_dsi; i++) {
		ret = mipi_dsi_picture_parameter_set(pinfo->dsi[i], &pps);
		if (ret < 0)
			return dev_err_probe(dev, ret,
					     "failed to transmit DSC PPS on DSI%d\n",
					     i);
	}

	for (i = 0; !dbg_skip_compression_mode &&
		    i < DSI_NUM_MIN + pinfo->desc->is_dual_dsi; i++) {
		ret = mipi_dsi_compression_mode(pinfo->dsi[i], true);
		if (ret < 0)
			return dev_err_probe(dev, ret,
					     "failed to enable DSC on DSI%d\n",
					     i);
	}

	msleep(28);

	return 0;
}

static void nt36523_reset(struct panel_info *pinfo)
{
	/* Android qcom,mdss-dsi-reset-sequence: 0 11, 1 1, 0 3, 1 10.
	 * The reset GPIO is active-low in the mainline DTS, so logical values
	 * below produce the same physical low/high/low/high waveform.
	 */
	gpiod_set_value_cansleep(pinfo->reset_gpio, 1);
	usleep_range(11000, 11100);
	gpiod_set_value_cansleep(pinfo->reset_gpio, 0);
	usleep_range(1000, 1100);
	gpiod_set_value_cansleep(pinfo->reset_gpio, 1);
	usleep_range(3000, 3100);
	gpiod_set_value_cansleep(pinfo->reset_gpio, 0);
	usleep_range(10000, 10100);
}

static void nt36523_power_off(struct panel_info *pinfo)
{
	regulator_disable(pinfo->enn);
	usleep_range(5000, 5100);
	regulator_disable(pinfo->enp);
	usleep_range(5000, 5100);
	regulator_disable(pinfo->vddio);
}

static int nt36523_power_on(struct panel_info *pinfo)
{
	int ret;

	ret = regulator_enable(pinfo->vddio);
	if (ret)
		return ret;

	ret = regulator_enable(pinfo->enp);
	if (ret)
		goto disable_vddio;
	usleep_range(5000, 5100);

	ret = regulator_enable(pinfo->enn);
	if (ret)
		goto disable_enp;
	usleep_range(11000, 11100);

	return 0;

disable_enp:
	regulator_disable(pinfo->enp);
disable_vddio:
	regulator_disable(pinfo->vddio);
	return ret;
}

/*
 * Read while the video engine is still off. Android answers these in HS mode:
 * 0a=9c 0f=c0 05=00 54=2c 52=07xx 90=83 92=11 00.
 */
static void nt36523_read_status(struct panel_info *pinfo)
{
	static const struct { u8 reg, len; } regs[] = {
		{ 0x0a, 1 }, { 0x0e, 1 }, { 0x0f, 1 }, { 0x05, 1 }, { 0x54, 1 },
		{ 0xda, 1 }, { 0x52, 2 }, { 0x90, 1 }, { 0x92, 2 }, { 0x91, 16 },
	};
	struct device *dev = pinfo->panel.dev;
	int i, r, ret;
	u8 out[16];

	if (!dbg_read_status)
		return;

	for (i = 0; i < DSI_NUM_MIN + pinfo->desc->is_dual_dsi; i++) {
		struct mipi_dsi_device *dsi = pinfo->dsi[i];
		unsigned long flags = dsi->mode_flags;

		if (dbg_read_hs)
			dsi->mode_flags &= ~MIPI_DSI_MODE_LPM;
		for (r = 0; r < ARRAY_SIZE(regs); r++) {
			memset(out, 0, sizeof(out));
			ret = mipi_dsi_dcs_read(dsi, regs[r].reg, out, regs[r].len);
			dev_info(dev, "status dsi%d %02x %s ret=%d: %*ph\n", i, regs[r].reg,
				 dbg_read_hs ? "hs" : "lp", ret, regs[r].len, out);
		}
		dsi->mode_flags = flags;
	}
}

static int nt36523_prepare(struct drm_panel *panel)
{
	struct panel_info *pinfo = to_panel_info(panel);
	int ret;

	ret = nt36523_power_on(pinfo);
	if (ret) {
		dev_err(panel->dev, "failed to enable panel supplies: %d\n", ret);
		return ret;
	}

	nt36523_reset(pinfo);
	dbg_pinfo = pinfo;

	if (pinfo->desc->has_dsc && dbg_pps_before_init) {
		ret = nt36523_enable_dsc(pinfo);
		if (ret < 0) {
			nt36523_power_off(pinfo);
			return ret;
		}
	}

	ret = pinfo->desc->init_sequence(pinfo);
	if (ret < 0) {
		nt36523_power_off(pinfo);
		dev_err(panel->dev, "failed to initialize panel: %d\n", ret);
		return ret;
	}

	/*
	 * Android and the other 10 bpc, two-slice Novatek panels turn the
	 * display on first and send the PPS afterwards.
	 */
	if (pinfo->desc->display_on_after_dsc && dbg_pps_after_display_on) {
		ret = kirby_csot_display_on_sequence(pinfo);
		if (ret < 0) {
			nt36523_power_off(pinfo);
			dev_err(panel->dev, "failed to set display on: %d\n", ret);
			return ret;
		}
		msleep(40);
	}

	if (pinfo->desc->has_dsc && !dbg_pps_before_init) {
		ret = nt36523_enable_dsc(pinfo);
		if (ret < 0) {
			nt36523_power_off(pinfo);
			return ret;
		}
	}

	if (pinfo->desc->display_on_after_dsc && !dbg_pps_after_display_on) {
		ret = kirby_csot_display_on_sequence(pinfo);
		if (ret < 0) {
			nt36523_power_off(pinfo);
			dev_err(panel->dev, "failed to set display on: %d\n", ret);
			return ret;
		}
	}


	nt36523_read_status(pinfo);

	return 0;
}

static int nt36523_disable(struct drm_panel *panel)
{
	struct panel_info *pinfo = to_panel_info(panel);
	int i, ret;

	for (i = 0; i < DSI_NUM_MIN + pinfo->desc->is_dual_dsi; i++) {
		ret = mipi_dsi_dcs_set_display_off(pinfo->dsi[i]);
		if (ret < 0)
			dev_err(&pinfo->dsi[i]->dev, "failed to set display off: %d\n", ret);
	}

	for (i = 0; i < DSI_NUM_MIN + pinfo->desc->is_dual_dsi; i++) {
		ret = mipi_dsi_dcs_enter_sleep_mode(pinfo->dsi[i]);
		if (ret < 0)
			dev_err(&pinfo->dsi[i]->dev, "failed to enter sleep mode: %d\n", ret);
	}

	msleep(70);

	return 0;
}

static int nt36523_unprepare(struct drm_panel *panel)
{
	struct panel_info *pinfo = to_panel_info(panel);

	gpiod_set_value_cansleep(pinfo->reset_gpio, 1);
	nt36523_power_off(pinfo);

	return 0;
}

static void nt36523_remove(struct mipi_dsi_device *dsi)
{
	struct panel_info *pinfo = mipi_dsi_get_drvdata(dsi);
	int ret;

	ret = mipi_dsi_detach(pinfo->dsi[0]);
	if (ret < 0)
		dev_err(&dsi->dev, "failed to detach from DSI0 host: %d\n", ret);

	if (pinfo->desc->is_dual_dsi) {
		ret = mipi_dsi_detach(pinfo->dsi[1]);
		if (ret < 0)
			dev_err(&pinfo->dsi[1]->dev, "failed to detach from DSI1 host: %d\n", ret);
		mipi_dsi_device_unregister(pinfo->dsi[1]);
	}

	drm_panel_remove(&pinfo->panel);
}

static int nt36523_get_modes(struct drm_panel *panel,
			       struct drm_connector *connector)
{
	struct panel_info *pinfo = to_panel_info(panel);
	int i;

	for (i = 0; i < pinfo->desc->num_modes; i++) {
		const struct drm_display_mode *m = &pinfo->desc->modes[i];
		struct drm_display_mode *mode;

		mode = drm_mode_duplicate(connector->dev, m);
		if (!mode) {
			dev_err(panel->dev, "failed to add mode %ux%u@%u\n",
				m->hdisplay, m->vdisplay, drm_mode_vrefresh(m));
			return -ENOMEM;
		}

		mode->type = DRM_MODE_TYPE_DRIVER;
		if (i == 0)
			mode->type |= DRM_MODE_TYPE_PREFERRED;

		drm_mode_set_name(mode);
		drm_mode_probed_add(connector, mode);
	}

	connector->display_info.width_mm = pinfo->desc->width_mm;
	connector->display_info.height_mm = pinfo->desc->height_mm;
	connector->display_info.bpc = pinfo->desc->bpc;

	return pinfo->desc->num_modes;
}

static enum drm_panel_orientation nt36523_get_orientation(struct drm_panel *panel)
{
	struct panel_info *pinfo = to_panel_info(panel);

	return pinfo->orientation;
}

static const struct drm_panel_funcs nt36523_panel_funcs = {
	.disable = nt36523_disable,
	.prepare = nt36523_prepare,
	.unprepare = nt36523_unprepare,
	.get_modes = nt36523_get_modes,
	.get_orientation = nt36523_get_orientation,
};

static int nt36523_bl_update_status(struct backlight_device *bl)
{
	struct mipi_dsi_device *dsi = bl_get_data(bl);
	u16 brightness = backlight_get_brightness(bl);
	int ret;

	dsi->mode_flags &= ~MIPI_DSI_MODE_LPM;

	ret = mipi_dsi_dcs_set_display_brightness_large(dsi, brightness);
	if (ret < 0)
		return ret;

	dsi->mode_flags |= MIPI_DSI_MODE_LPM;

	return 0;
}

static int nt36523_bl_get_brightness(struct backlight_device *bl)
{
	struct mipi_dsi_device *dsi = bl_get_data(bl);
	u16 brightness;
	int ret;

	dsi->mode_flags &= ~MIPI_DSI_MODE_LPM;

	ret = mipi_dsi_dcs_get_display_brightness_large(dsi, &brightness);
	if (ret < 0)
		return ret;

	dsi->mode_flags |= MIPI_DSI_MODE_LPM;

	return brightness;
}

static const struct backlight_ops nt36523_bl_ops = {
	.update_status = nt36523_bl_update_status,
	.get_brightness = nt36523_bl_get_brightness,
};

static struct backlight_device *nt36523_create_backlight(struct mipi_dsi_device *dsi)
{
	struct device *dev = &dsi->dev;
	const struct backlight_properties props = {
		.type = BACKLIGHT_RAW,
		.brightness = 512,
		.max_brightness = 4095,
		.scale = BACKLIGHT_SCALE_NON_LINEAR,
	};

	return devm_backlight_device_register(dev, dev_name(dev), dev, dsi,
					      &nt36523_bl_ops, &props);
}

static int nt36523_probe(struct mipi_dsi_device *dsi)
{
	struct device *dev = &dsi->dev;
	struct device_node *dsi1;
	struct mipi_dsi_host *dsi1_host;
	struct panel_info *pinfo;
	const struct mipi_dsi_device_info *info;
	int i, ret;

#if LINUX_VERSION_CODE >= KERNEL_VERSION(7, 2, 0)
	pinfo = devm_drm_panel_alloc(dev, struct panel_info, panel,
				     &nt36523_panel_funcs,
				     DRM_MODE_CONNECTOR_DSI);
	if (IS_ERR(pinfo))
		return PTR_ERR(pinfo);
#else
	pinfo = devm_kzalloc(dev, sizeof(*pinfo), GFP_KERNEL);
	if (!pinfo)
		return -ENOMEM;
#endif

	pinfo->vddio = devm_regulator_get(dev, "vddio");
	if (IS_ERR(pinfo->vddio))
		return dev_err_probe(dev, PTR_ERR(pinfo->vddio), "failed to get vddio regulator\n");
	pinfo->enp = devm_regulator_get(dev, "enp");
	if (IS_ERR(pinfo->enp))
		return dev_err_probe(dev, PTR_ERR(pinfo->enp), "failed to get enp regulator\n");

	pinfo->enn = devm_regulator_get(dev, "enn");
	if (IS_ERR(pinfo->enn))
		return dev_err_probe(dev, PTR_ERR(pinfo->enn), "failed to get enn regulator\n");

	pinfo->reset_gpio = devm_gpiod_get(dev, "reset", GPIOD_OUT_LOW);
	if (IS_ERR(pinfo->reset_gpio))
		return dev_err_probe(dev, PTR_ERR(pinfo->reset_gpio), "failed to get reset gpio\n");

	pinfo->desc = of_device_get_match_data(dev);
	if (!pinfo->desc)
		return -ENODEV;

	if (pinfo->desc->has_dsc) {
		unsigned int slices_per_link = pinfo->desc->dsc_slice_per_pkt ? : 1;

		pinfo->dsc.dsc_version_major = 1;
		pinfo->dsc.dsc_version_minor = 1;
		pinfo->dsc.pic_width = pinfo->desc->dsc_pic_width ? : 1600;
		pinfo->dsc.pic_height = pinfo->desc->dsc_pic_height ? : 2560;
		pinfo->dsc.slice_height = pinfo->desc->dsc_slice_height ? : 20;
		pinfo->dsc.slice_width = pinfo->desc->dsc_slice_width ? : 800;
		/*
		 * MSM DPU uses pic_width/slice_width for the full bonded frame,
		 * while the DSI host programs packet timing from slice_count.
		 * Match the downstream per-controller view:
		 * - Kirby (TB321FU): 1 x 800px slice per DSI link
		 * - Lapis (TB520FU): 2 x 736px slices (1472px) per DSI link
		 */
		pinfo->dsc.slice_count = slices_per_link;
		pinfo->dsc.bits_per_component = pinfo->desc->bpc ? : 8;
		pinfo->dsc.bits_per_pixel = 8 << 4;
		pinfo->dsc.block_pred_enable = true;
		/* Matches the fixed settings written to register 0x91 at init. */
		pinfo->dsc.first_line_bpg_offset =
			pinfo->desc->dsc_first_line_bpg_offset;
	}

	/* If the panel is dual dsi, register DSI1 */
	if (pinfo->desc->is_dual_dsi) {
		info = &pinfo->desc->dsi_info;

		dsi1 = of_graph_get_remote_node(dsi->dev.of_node, 1, -1);
		if (!dsi1) {
			dev_err(dev, "cannot get secondary DSI node.\n");
			return -ENODEV;
		}

		dsi1_host = of_find_mipi_dsi_host_by_node(dsi1);
		of_node_put(dsi1);
		if (!dsi1_host)
			return dev_err_probe(dev, -EPROBE_DEFER, "cannot get secondary DSI host\n");

		pinfo->dsi[1] = mipi_dsi_device_register_full(dsi1_host, info);
		if (IS_ERR(pinfo->dsi[1])) {
			dev_err(dev, "cannot get secondary DSI device\n");
			return PTR_ERR(pinfo->dsi[1]);
		}
	}

	pinfo->dsi[0] = dsi;
	mipi_dsi_set_drvdata(dsi, pinfo);
	pinfo->panel.prepare_prev_first = true;
#if LINUX_VERSION_CODE < KERNEL_VERSION(7, 2, 0)
	drm_panel_init(&pinfo->panel, dev, &nt36523_panel_funcs, DRM_MODE_CONNECTOR_DSI);
	ret = of_drm_get_panel_orientation(dev->of_node, &pinfo->orientation);
#else
	ret = drm_of_get_panel_orientation(dev->of_node, &pinfo->orientation);
#endif
	if (ret < 0) {
		dev_err(dev, "%pOF: failed to get orientation %d\n", dev->of_node, ret);
		return ret;
	}

	pinfo->panel.prepare_prev_first = true;

	if (pinfo->desc->has_dcs_backlight) {
		pinfo->panel.backlight = nt36523_create_backlight(dsi);
		if (IS_ERR(pinfo->panel.backlight))
			return dev_err_probe(dev, PTR_ERR(pinfo->panel.backlight),
					     "Failed to create backlight\n");
	} else {
		ret = drm_panel_of_backlight(&pinfo->panel);
		if (ret)
			return dev_err_probe(dev, ret, "Failed to get backlight\n");
	}

	drm_panel_add(&pinfo->panel);

	for (i = 0; i < DSI_NUM_MIN + pinfo->desc->is_dual_dsi; i++) {
		pinfo->dsi[i]->lanes = pinfo->desc->lanes;
		pinfo->dsi[i]->format = pinfo->desc->format;
		pinfo->dsi[i]->mode_flags = pinfo->desc->mode_flags;
		if (pinfo->desc->has_dsc) {
			pinfo->dsi[i]->dsc = &pinfo->dsc;
			pinfo->dsi[i]->dsc_slice_per_pkt =
				pinfo->desc->dsc_slice_per_pkt ? : 1;
		}

		ret = mipi_dsi_attach(pinfo->dsi[i]);
		if (ret < 0)
			return dev_err_probe(dev, ret, "cannot attach to DSI%d host.\n", i);
	}

	return 0;
}

static const struct of_device_id nt36523_of_match[] = {
	{
		.compatible = "lenovo,tb520fu-lapis-nt36532-tianma-dsc-vid",
		.data = &lapis_tianma_desc,
	},
	{
		.compatible = "lenovo,lapis-nt36532-tianma-dsc-vid",
		.data = &lapis_tianma_desc,
	},
	{
		.compatible = "lenovo,tb520fu-lapis-nt36532-boe-dsc-vid",
		.data = &lapis_boe_desc,
	},
	{
		.compatible = "lenovo,lapis-nt36532-boe-dsc-vid",
		.data = &lapis_boe_desc,
	},
	{
		.compatible = "lenovo,tb321fu-kirby-nt36523-csot-dsc-vid",
		.data = &kirby_csot_desc,
	},
	{
		.compatible = "lenovo,kirby-nt36523-csot-dsc-vid",
		.data = &kirby_csot_desc,
	},
	{
		.compatible = "lenovo,tb321fu-kirby-nt36523-csot-dsc",
		.data = &kirby_csot_desc,
	},
	{
		.compatible = "lenovo,kirby-nt36523-csot-dsc",
		.data = &kirby_csot_desc,
	},
	{
		.compatible = "lenovo,j606f-boe-nt36523w",
		.data = &j606f_boe_desc,
	},
	{
		.compatible = "xiaomi,elish-boe-nt36523",
		.data = &elish_boe_desc,
	},
	{
		.compatible = "xiaomi,elish-csot-nt36523",
		.data = &elish_csot_desc,
	},
	{
		.compatible = "xiaomi,nabu-csot-nt36523",
		.data = &nabu_csot_desc,
	},
	{},
};
MODULE_DEVICE_TABLE(of, nt36523_of_match);

static struct mipi_dsi_driver nt36523_driver = {
	.probe = nt36523_probe,
	.remove = nt36523_remove,
	.driver = {
		.name = "panel-novatek-nt36523",
		.of_match_table = nt36523_of_match,
	},
};
module_mipi_dsi_driver(nt36523_driver);

MODULE_AUTHOR("Jianhua Lu <lujianhua000@gmail.com>");
MODULE_DESCRIPTION("DRM driver for Novatek NT36523 based MIPI DSI panels");
MODULE_LICENSE("GPL");
