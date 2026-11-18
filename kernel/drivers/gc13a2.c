// SPDX-License-Identifier: GPL-2.0
/*
 * V4L2 subdev driver for the GalaxyCore GC13A2 image sensor
 * on Lenovo Yoga Pad Pro 12.7 (TB520FU / SM8650 Lapis).
 */
#include <linux/array_size.h>
#include <linux/bits.h>
#include <linux/clk.h>
#include <linux/container_of.h>
#include <linux/delay.h>
#include <linux/device.h>
#include <linux/err.h>
#include <linux/gpio/consumer.h>
#include <linux/math64.h>
#include <linux/minmax.h>
#include <linux/mod_devicetable.h>
#include <linux/module.h>
#include <linux/pm_runtime.h>
#include <linux/property.h>
#include <linux/regulator/consumer.h>
#include <linux/types.h>
#include <linux/units.h>

#include <media/v4l2-cci.h>
#include <media/v4l2-ctrls.h>
#include <media/v4l2-fwnode.h>
#include <media/v4l2-subdev.h>

#define GC13A2_REG_TEST_PATTERN		CCI_REG8(0x00ce)
#define GC13A2_REG_TEST_PATTERN_VAL	CCI_REG8(0x00cf)
#define GC13A2_TEST_PATTERN_OFF		0x09
#define GC13A2_TEST_PATTERN_BLACK	0x10

#define GC13A2_STREAMING_REG		CCI_REG8(0x0100)
#define GC13A2_STREAM_ON		0x01
#define GC13A2_STREAM_OFF		0x80

#define GC13A2_FLIP_REG			CCI_REG8(0x0101)
#define GC13A2_FLIP_H_MASK		BIT(0)
#define GC13A2_FLIP_V_MASK		BIT(1)

#define GC13A2_EXP_REG			CCI_REG16(0x0202)
#define GC13A2_EXP_MARGIN		16
#define GC13A2_EXP_MIN			4
#define GC13A2_EXP_STEP			1
#define GC13A2_EXP_DEFAULT		0x0b64

/*
 * GC13A2 encodes 10-bit fixed-point analogue gain (1.0x = 1024, 8.0x = 8192)
 * across 24-bit registers 0x0204..0x0206 as ((gain << 3) & 0xfffff8), so
 * 1.0x = 0x002000 and 8.0x = 0x010000.
 */
#define GC13A2_AGAIN_REG		CCI_REG24(0x0204)
#define GC13A2_AGAIN_MIN		1024
#define GC13A2_AGAIN_MAX		(1024 * 8)
#define GC13A2_AGAIN_STEP		1

#define GC13A2_FRAME_LENGTH_REG		CCI_REG16(0x0340)
#define GC13A2_VTS_MAX			0xffee

#define GC13A2_REG_CHIP_ID		CCI_REG16(0x03f0)
#define GC13A2_CHIP_ID			0x13a2
#define GC13A2_CHIP_ID_LCE		0x13ac
#define GC13A2_CHIP_ID_TXD		0x13ad

#define GC13A2_NATIVE_WIDTH		4208
#define GC13A2_NATIVE_HEIGHT		3120

#define GC13A2_DEFAULT_CLK_FREQ		19200000
#define GC13A2_MBUS_CODE		MEDIA_BUS_FMT_SGRBG10_1X10
#define GC13A2_DATA_LANES		4
#define GC13A2_RGB_DEPTH		10

static const char *const gc13a2_test_pattern_menu[] = {
	"Disabled",
	"Solid Black",
};

static const s64 gc13a2_link_freq_menu_items[] = {
	595200000ULL,
	297600000ULL,
};

static const char *const gc13a2_supply_name[] = {
	"dovdd",
	"dvdd",
	"avdd",
};

struct gc13a2_reg_list {
	u32 num_of_regs;
	const struct cci_reg_sequence *regs;
};

#include "gc13a2-lapis-regs.h"

struct gc13a2_mode {
	u32 width;
	u32 height;
	u32 crop_top;
	const struct gc13a2_reg_list reg_list;
	u32 hts;
	u32 vts_def;
	u32 vts_min;
	u32 link_freq_idx;
};

static const struct gc13a2_mode gc13a2_modes[] = {
	{
		.width = 4208,
		.height = 3120,
		.crop_top = 0,
		.reg_list = {
			.num_of_regs = ARRAY_SIZE(gc13a2_lapis_mode_4208x3120_regs),
			.regs = gc13a2_lapis_mode_4208x3120_regs,
		},
		.hts = 4224,
		.vts_def = 3248,
		.vts_min = 3248,
		.link_freq_idx = 0,
	},
	{
		.width = 4208,
		.height = 2368,
		.crop_top = 376,
		.reg_list = {
			.num_of_regs = ARRAY_SIZE(gc13a2_lapis_mode_4208x2368_regs),
			.regs = gc13a2_lapis_mode_4208x2368_regs,
		},
		.hts = 4224,
		.vts_def = 3248,
		.vts_min = 3248,
		.link_freq_idx = 0,
	},
	{
		.width = 2104,
		.height = 1560,
		.crop_top = 0,
		.reg_list = {
			.num_of_regs = ARRAY_SIZE(gc13a2_lapis_mode_2104x1560_regs),
			.regs = gc13a2_lapis_mode_2104x1560_regs,
		},
		.hts = 4224,
		.vts_def = 3240,
		.vts_min = 3240,
		.link_freq_idx = 1,
	},
};

struct gc13a2 {
	struct device *dev;
	struct v4l2_subdev sd;
	struct media_pad pad;

	struct clk *xclk;
	struct regulator_bulk_data supplies[ARRAY_SIZE(gc13a2_supply_name)];
	struct gpio_desc *reset_gpio;

	struct v4l2_ctrl_handler ctrls;
	struct v4l2_ctrl *pixel_rate;
	struct v4l2_ctrl *link_freq;
	struct v4l2_ctrl *exposure;
	struct v4l2_ctrl *vblank;
	struct v4l2_ctrl *hblank;
	struct v4l2_ctrl *hflip;
	struct v4l2_ctrl *vflip;

	struct regmap *regmap;
	unsigned long link_freq_bitmap;
	const struct gc13a2_mode *cur_mode;
};

static inline struct gc13a2 *to_gc13a2(struct v4l2_subdev *sd)
{
	return container_of(sd, struct gc13a2, sd);
}

static u64 gc13a2_to_pixel_rate(u32 f_index)
{
	u64 pixel_rate =
		gc13a2_link_freq_menu_items[f_index] * 2 * GC13A2_DATA_LANES;

	return div_u64(pixel_rate, GC13A2_RGB_DEPTH);
}

static int gc13a2_power_on(struct device *dev)
{
	struct v4l2_subdev *sd = dev_get_drvdata(dev);
	struct gc13a2 *gc13a2 = to_gc13a2(sd);
	int ret, i;

	gpiod_set_value_cansleep(gc13a2->reset_gpio, 1);

	for (i = 0; i < ARRAY_SIZE(gc13a2_supply_name); i++) {
		ret = regulator_enable(gc13a2->supplies[i].consumer);
		if (ret < 0) {
			dev_err(gc13a2->dev, "failed to enable %s: %d\n",
				gc13a2_supply_name[i], ret);
			goto disable_supplies;
		}
		fsleep(1000);
	}

	gpiod_set_value_cansleep(gc13a2->reset_gpio, 0);
	fsleep(2000);

	ret = clk_prepare_enable(gc13a2->xclk);
	if (ret < 0) {
		dev_err(gc13a2->dev, "clk prepare enable failed: %d\n", ret);
		gpiod_set_value_cansleep(gc13a2->reset_gpio, 1);
		goto disable_supplies;
	}

	fsleep(6000);

	return 0;

disable_supplies:
	while (--i >= 0)
		regulator_disable(gc13a2->supplies[i].consumer);

	return ret;
}

static int gc13a2_power_off(struct device *dev)
{
	struct v4l2_subdev *sd = dev_get_drvdata(dev);
	struct gc13a2 *gc13a2 = to_gc13a2(sd);
	int i;

	clk_disable_unprepare(gc13a2->xclk);
	fsleep(2000);
	gpiod_set_value_cansleep(gc13a2->reset_gpio, 1);
	fsleep(1000);

	for (i = ARRAY_SIZE(gc13a2_supply_name) - 1; i >= 0; i--)
		regulator_disable(gc13a2->supplies[i].consumer);

	return 0;
}

static int gc13a2_enum_mbus_code(struct v4l2_subdev *sd,
				 struct v4l2_subdev_state *sd_state,
				 struct v4l2_subdev_mbus_code_enum *code)
{
	if (code->index > 0)
		return -EINVAL;

	code->code = GC13A2_MBUS_CODE;

	return 0;
}

static int gc13a2_enum_frame_size(struct v4l2_subdev *subdev,
				  struct v4l2_subdev_state *sd_state,
				  struct v4l2_subdev_frame_size_enum *fse)
{
	struct gc13a2 *gc13a2 = to_gc13a2(subdev);
	unsigned int idx = 0, i;

	if (fse->code != GC13A2_MBUS_CODE)
		return -EINVAL;

	for (i = 0; i < ARRAY_SIZE(gc13a2_modes); i++) {
		if (!(gc13a2->link_freq_bitmap & BIT(gc13a2_modes[i].link_freq_idx)))
			continue;
		if (idx == fse->index) {
			fse->min_width = gc13a2_modes[i].width;
			fse->max_width = gc13a2_modes[i].width;
			fse->min_height = gc13a2_modes[i].height;
			fse->max_height = gc13a2_modes[i].height;
			return 0;
		}
		idx++;
	}

	return -EINVAL;
}

static int gc13a2_update_cur_mode_controls(struct gc13a2 *gc13a2,
					   const struct gc13a2_mode *mode)
{
	s64 exposure_max, exposure_def, h_blank, pixel_rate;
	int ret;

	ret = __v4l2_ctrl_s_ctrl(gc13a2->link_freq, mode->link_freq_idx);
	if (ret)
		return ret;

	pixel_rate = gc13a2_to_pixel_rate(mode->link_freq_idx);
	ret = __v4l2_ctrl_modify_range(gc13a2->pixel_rate, pixel_rate,
				       pixel_rate, 1, pixel_rate);
	if (ret)
		return ret;

	ret = __v4l2_ctrl_modify_range(gc13a2->vblank,
				       mode->vts_min - mode->height,
				       GC13A2_VTS_MAX - mode->height, 1,
				       mode->vts_def - mode->height);
	if (ret)
		return ret;

	h_blank = mode->hts - mode->width;
	ret = __v4l2_ctrl_modify_range(gc13a2->hblank, h_blank, h_blank, 1,
				       h_blank);
	if (ret)
		return ret;

	exposure_max = mode->vts_def - GC13A2_EXP_MARGIN;
	exposure_def = min_t(s64, GC13A2_EXP_DEFAULT, exposure_max);
	ret = __v4l2_ctrl_modify_range(gc13a2->exposure, GC13A2_EXP_MIN,
				       exposure_max, GC13A2_EXP_STEP,
				       exposure_def);
	if (ret)
		return ret;

	return 0;
}

static void gc13a2_update_pad_format(const struct gc13a2_mode *mode,
				     struct v4l2_mbus_framefmt *fmt)
{
	fmt->width = mode->width;
	fmt->height = mode->height;
	fmt->code = GC13A2_MBUS_CODE;
	fmt->field = V4L2_FIELD_NONE;
	fmt->colorspace = V4L2_COLORSPACE_RAW;
	fmt->ycbcr_enc = V4L2_MAP_YCBCR_ENC_DEFAULT(fmt->colorspace);
	fmt->quantization = V4L2_QUANTIZATION_FULL_RANGE;
	fmt->xfer_func = V4L2_XFER_FUNC_NONE;
}

static const struct gc13a2_mode *gc13a2_find_best_mode(struct gc13a2 *gc13a2,
						       u32 width, u32 height)
{
	const struct gc13a2_mode *best = &gc13a2_modes[0];
	u64 best_err = U64_MAX;
	unsigned int i;

	for (i = 0; i < ARRAY_SIZE(gc13a2_modes); i++) {
		u64 dw, dh, err;

		if (!(gc13a2->link_freq_bitmap & BIT(gc13a2_modes[i].link_freq_idx)))
			continue;

		dw = abs_diff(gc13a2_modes[i].width, width);
		dh = abs_diff(gc13a2_modes[i].height, height);
		err = dw + dh;
		if (err < best_err) {
			best_err = err;
			best = &gc13a2_modes[i];
		}
	}

	return best;
}

static int gc13a2_set_format(struct v4l2_subdev *sd,
			     struct v4l2_subdev_state *state,
			     struct v4l2_subdev_format *fmt)
{
	struct gc13a2 *gc13a2 = to_gc13a2(sd);
	struct v4l2_mbus_framefmt *mbus_fmt;
	struct v4l2_rect *crop;
	const struct gc13a2_mode *mode;

	mode = gc13a2_find_best_mode(gc13a2, fmt->format.width,
				     fmt->format.height);

	crop = v4l2_subdev_state_get_crop(state, 0);
	crop->left = 0;
	crop->top = mode->crop_top;
	crop->width = mode->width;
	crop->height = mode->height;

	gc13a2_update_pad_format(mode, &fmt->format);
	mbus_fmt = v4l2_subdev_state_get_format(state, 0);
	*mbus_fmt = fmt->format;

	if (fmt->which == V4L2_SUBDEV_FORMAT_TRY)
		return 0;

	gc13a2->cur_mode = mode;
	return gc13a2_update_cur_mode_controls(gc13a2, mode);
}

static int gc13a2_get_selection(struct v4l2_subdev *sd,
				struct v4l2_subdev_state *state,
				struct v4l2_subdev_selection *sel)
{
	switch (sel->target) {
	case V4L2_SEL_TGT_CROP_DEFAULT:
	case V4L2_SEL_TGT_CROP:
		sel->r = *v4l2_subdev_state_get_crop(state, 0);
		break;
	case V4L2_SEL_TGT_CROP_BOUNDS:
		sel->r.top = 0;
		sel->r.left = 0;
		sel->r.width = GC13A2_NATIVE_WIDTH;
		sel->r.height = GC13A2_NATIVE_HEIGHT;
		break;
	default:
		return -EINVAL;
	}

	return 0;
}

static int gc13a2_init_state(struct v4l2_subdev *sd,
			     struct v4l2_subdev_state *state)
{
	struct v4l2_subdev_format fmt = {
		.which = V4L2_SUBDEV_FORMAT_TRY,
		.pad = 0,
		.format = {
			.code = GC13A2_MBUS_CODE,
			.width = gc13a2_modes[0].width,
			.height = gc13a2_modes[0].height,
		},
	};

	return gc13a2_set_format(sd, state, &fmt);
}

static int gc13a2_test_pattern(struct gc13a2 *gc13a2, u32 pattern_menu)
{
	int ret;

	if (!pattern_menu)
		return cci_write(gc13a2->regmap, GC13A2_REG_TEST_PATTERN,
				 GC13A2_TEST_PATTERN_OFF, NULL);

	ret = cci_write(gc13a2->regmap, GC13A2_REG_TEST_PATTERN,
			GC13A2_TEST_PATTERN_BLACK, NULL);
	if (ret)
		return ret;

	return cci_write(gc13a2->regmap, GC13A2_REG_TEST_PATTERN_VAL,
			 0x00, NULL);
}

static int gc13a2_set_ctrl(struct v4l2_ctrl *ctrl)
{
	struct gc13a2 *gc13a2 =
		container_of(ctrl->handler, struct gc13a2, ctrls);
	struct v4l2_subdev_state *state;
	const struct v4l2_mbus_framefmt *format;
	s64 exposure_max, exposure_def;
	u64 encoded_gain;
	int ret = 0;

	state = v4l2_subdev_get_locked_active_state(&gc13a2->sd);
	format = v4l2_subdev_state_get_format(state, 0);

	if (ctrl->id == V4L2_CID_VBLANK) {
		exposure_max = format->height + ctrl->val - GC13A2_EXP_MARGIN;
		exposure_def = min_t(s64, gc13a2->exposure->val, exposure_max);
		__v4l2_ctrl_modify_range(gc13a2->exposure,
					 gc13a2->exposure->minimum,
					 exposure_max, gc13a2->exposure->step,
					 exposure_def);
	}

	if (!pm_runtime_get_if_active(gc13a2->dev))
		return 0;

	switch (ctrl->id) {
	case V4L2_CID_EXPOSURE:
		ret = cci_write(gc13a2->regmap, GC13A2_EXP_REG,
				ctrl->val, NULL);
		break;

	case V4L2_CID_ANALOGUE_GAIN:
		encoded_gain = ((u64)(u32)ctrl->val << 3) & 0xfffff8ULL;
		ret = cci_write(gc13a2->regmap, GC13A2_AGAIN_REG,
				encoded_gain, NULL);
		break;

	case V4L2_CID_VBLANK:
		ret = cci_write(gc13a2->regmap, GC13A2_FRAME_LENGTH_REG,
				gc13a2->cur_mode->height + ctrl->val, NULL);
		break;

	case V4L2_CID_HFLIP:
		ret = cci_update_bits(gc13a2->regmap, GC13A2_FLIP_REG,
				      GC13A2_FLIP_H_MASK,
				      ctrl->val ? GC13A2_FLIP_H_MASK : 0, NULL);
		break;

	case V4L2_CID_VFLIP:
		ret = cci_update_bits(gc13a2->regmap, GC13A2_FLIP_REG,
				      GC13A2_FLIP_V_MASK,
				      ctrl->val ? GC13A2_FLIP_V_MASK : 0, NULL);
		break;

	case V4L2_CID_TEST_PATTERN:
		ret = gc13a2_test_pattern(gc13a2, ctrl->val);
		break;

	default:
		break;
	}

	pm_runtime_put(gc13a2->dev);

	return ret;
}

static const struct v4l2_ctrl_ops gc13a2_ctrl_ops = {
	.s_ctrl = gc13a2_set_ctrl,
};

static int gc13a2_start_streaming(struct gc13a2 *gc13a2)
{
	const struct gc13a2_reg_list *reg_list;
	const struct gc13a2_mode *mode;
	int ret;

	ret = pm_runtime_resume_and_get(gc13a2->dev);
	if (ret < 0)
		return ret;

	ret = cci_multi_reg_write(gc13a2->regmap, gc13a2_lapis_init_regs,
				  ARRAY_SIZE(gc13a2_lapis_init_regs), NULL);
	if (ret)
		goto err_rpm_put;

	mode = gc13a2->cur_mode;
	reg_list = &mode->reg_list;
	ret = cci_multi_reg_write(gc13a2->regmap, reg_list->regs,
				  reg_list->num_of_regs, NULL);
	if (ret)
		goto err_rpm_put;

	ret = __v4l2_ctrl_handler_setup(&gc13a2->ctrls);
	if (ret) {
		dev_err(gc13a2->dev, "could not sync v4l2 controls: %d\n", ret);
		goto err_rpm_put;
	}

	ret = cci_write(gc13a2->regmap, GC13A2_STREAMING_REG,
			GC13A2_STREAM_ON, NULL);
	if (ret) {
		dev_err(gc13a2->dev, "write stream on failed: %d\n", ret);
		goto err_rpm_put;
	}

	return 0;

err_rpm_put:
	pm_runtime_put(gc13a2->dev);
	return ret;
}

static int gc13a2_stop_streaming(struct gc13a2 *gc13a2)
{
	int ret;

	ret = cci_write(gc13a2->regmap, GC13A2_STREAMING_REG,
			GC13A2_STREAM_OFF, NULL);
	if (ret)
		dev_err(gc13a2->dev, "could not stop streaming: %d\n", ret);

	pm_runtime_put(gc13a2->dev);

	return ret;
}

static int gc13a2_s_stream(struct v4l2_subdev *subdev, int enable)
{
	struct gc13a2 *gc13a2 = to_gc13a2(subdev);
	struct v4l2_subdev_state *state;
	int ret;

	state = v4l2_subdev_lock_and_get_active_state(subdev);

	if (enable)
		ret = gc13a2_start_streaming(gc13a2);
	else
		ret = gc13a2_stop_streaming(gc13a2);

	v4l2_subdev_unlock_state(state);

	return ret;
}

static const struct v4l2_subdev_video_ops gc13a2_video_ops = {
	.s_stream = gc13a2_s_stream,
};

static const struct v4l2_subdev_pad_ops gc13a2_subdev_pad_ops = {
	.enum_mbus_code = gc13a2_enum_mbus_code,
	.enum_frame_size = gc13a2_enum_frame_size,
	.get_fmt = v4l2_subdev_get_fmt,
	.set_fmt = gc13a2_set_format,
	.get_selection = gc13a2_get_selection,
};

static const struct v4l2_subdev_ops gc13a2_subdev_ops = {
	.video = &gc13a2_video_ops,
	.pad = &gc13a2_subdev_pad_ops,
};

static const struct v4l2_subdev_internal_ops gc13a2_internal_ops = {
	.init_state = gc13a2_init_state,
};

static int gc13a2_get_regulators(struct device *dev, struct gc13a2 *gc13a2)
{
	unsigned int i;

	for (i = 0; i < ARRAY_SIZE(gc13a2_supply_name); i++)
		gc13a2->supplies[i].supply = gc13a2_supply_name[i];

	return devm_regulator_bulk_get(dev, ARRAY_SIZE(gc13a2_supply_name),
				       gc13a2->supplies);
}

static int gc13a2_parse_fwnode(struct gc13a2 *gc13a2)
{
	struct fwnode_handle *endpoint;
	struct v4l2_fwnode_endpoint bus_cfg = {
		.bus_type = V4L2_MBUS_CSI2_DPHY,
	};
	struct device *dev = gc13a2->dev;
	int ret;

	endpoint = fwnode_graph_get_endpoint_by_id(dev_fwnode(dev), 0, 0,
						   FWNODE_GRAPH_ENDPOINT_NEXT);
	if (!endpoint) {
		dev_err(dev, "endpoint node not found\n");
		return -EINVAL;
	}

	ret = v4l2_fwnode_endpoint_alloc_parse(endpoint, &bus_cfg);
	if (ret) {
		dev_err(dev, "parsing endpoint node failed: %d\n", ret);
		goto done;
	}

	ret = v4l2_link_freq_to_bitmap(dev, bus_cfg.link_frequencies,
				       bus_cfg.nr_of_link_frequencies,
				       gc13a2_link_freq_menu_items,
				       ARRAY_SIZE(gc13a2_link_freq_menu_items),
				       &gc13a2->link_freq_bitmap);
	if (ret)
		goto done;

	if (!(gc13a2->link_freq_bitmap & BIT(0))) {
		dev_err(dev, "595.2 MHz link frequency is required\n");
		ret = -EINVAL;
	}

done:
	v4l2_fwnode_endpoint_free(&bus_cfg);
	fwnode_handle_put(endpoint);
	return ret;
}

static int gc13a2_init_controls(struct gc13a2 *gc13a2)
{
	struct i2c_client *client = v4l2_get_subdevdata(&gc13a2->sd);
	const struct gc13a2_mode *mode = &gc13a2_modes[0];
	const struct v4l2_ctrl_ops *ops = &gc13a2_ctrl_ops;
	struct v4l2_fwnode_device_properties props;
	struct v4l2_ctrl_handler *ctrl_hdlr;
	s64 exposure_max, exposure_def, h_blank, pixel_rate;
	int ret;

	ctrl_hdlr = &gc13a2->ctrls;
	ret = v4l2_ctrl_handler_init(ctrl_hdlr, 11);
	if (ret)
		return ret;

	gc13a2->hflip = v4l2_ctrl_new_std(ctrl_hdlr, ops, V4L2_CID_HFLIP,
					  0, 1, 1, 0);
	gc13a2->vflip = v4l2_ctrl_new_std(ctrl_hdlr, ops, V4L2_CID_VFLIP,
					  0, 1, 1, 0);

	gc13a2->link_freq =
		v4l2_ctrl_new_int_menu(ctrl_hdlr, ops, V4L2_CID_LINK_FREQ,
				       ARRAY_SIZE(gc13a2_link_freq_menu_items) - 1,
				       mode->link_freq_idx,
				       gc13a2_link_freq_menu_items);
	if (gc13a2->link_freq)
		gc13a2->link_freq->flags |= V4L2_CTRL_FLAG_READ_ONLY;

	pixel_rate = gc13a2_to_pixel_rate(mode->link_freq_idx);
	gc13a2->pixel_rate =
		v4l2_ctrl_new_std(ctrl_hdlr, ops, V4L2_CID_PIXEL_RATE,
				  gc13a2_to_pixel_rate(1),
				  gc13a2_to_pixel_rate(0), 1, pixel_rate);
	if (gc13a2->pixel_rate)
		gc13a2->pixel_rate->flags |= V4L2_CTRL_FLAG_READ_ONLY;

	gc13a2->vblank =
		v4l2_ctrl_new_std(ctrl_hdlr, ops, V4L2_CID_VBLANK,
				  mode->vts_min - mode->height,
				  GC13A2_VTS_MAX - mode->height, 1,
				  mode->vts_def - mode->height);

	h_blank = mode->hts - mode->width;
	gc13a2->hblank = v4l2_ctrl_new_std(ctrl_hdlr, ops, V4L2_CID_HBLANK,
					   h_blank, h_blank, 1, h_blank);
	if (gc13a2->hblank)
		gc13a2->hblank->flags |= V4L2_CTRL_FLAG_READ_ONLY;

	v4l2_ctrl_new_std(ctrl_hdlr, ops, V4L2_CID_ANALOGUE_GAIN,
			  GC13A2_AGAIN_MIN, GC13A2_AGAIN_MAX,
			  GC13A2_AGAIN_STEP, GC13A2_AGAIN_MIN);

	exposure_max = mode->vts_def - GC13A2_EXP_MARGIN;
	exposure_def = min_t(s64, GC13A2_EXP_DEFAULT, exposure_max);
	gc13a2->exposure = v4l2_ctrl_new_std(ctrl_hdlr, ops, V4L2_CID_EXPOSURE,
					     GC13A2_EXP_MIN, exposure_max,
					     GC13A2_EXP_STEP, exposure_def);

	v4l2_ctrl_new_std_menu_items(ctrl_hdlr, ops, V4L2_CID_TEST_PATTERN,
				     ARRAY_SIZE(gc13a2_test_pattern_menu) - 1,
				     0, 0, gc13a2_test_pattern_menu);

	ret = v4l2_fwnode_device_parse(&client->dev, &props);
	if (ret)
		goto error_ctrls;

	ret = v4l2_ctrl_new_fwnode_properties(ctrl_hdlr, ops, &props);
	if (ret)
		goto error_ctrls;

	if (ctrl_hdlr->error) {
		ret = ctrl_hdlr->error;
		goto error_ctrls;
	}

	gc13a2->sd.ctrl_handler = ctrl_hdlr;

	return 0;

error_ctrls:
	v4l2_ctrl_handler_free(ctrl_hdlr);

	return ret;
}

static int gc13a2_identify_module(struct gc13a2 *gc13a2)
{
	u64 val;
	int ret;

	ret = cci_read(gc13a2->regmap, GC13A2_REG_CHIP_ID, &val, NULL);
	if (ret) {
		dev_err(gc13a2->dev, "failed to read chip id: %d\n", ret);
		return ret;
	}

	if (val != GC13A2_CHIP_ID &&
	    val != GC13A2_CHIP_ID_LCE &&
	    val != GC13A2_CHIP_ID_TXD) {
		dev_err(gc13a2->dev, "chip id mismatch: 0x%04llx\n", val);
		return -ENXIO;
	}

	dev_info(gc13a2->dev, "GC13A2 detected (chip id 0x%04llx)\n", val);

	return 0;
}

static int gc13a2_probe(struct i2c_client *client)
{
	struct device *dev = &client->dev;
	struct gc13a2 *gc13a2;
	int ret;

	gc13a2 = devm_kzalloc(dev, sizeof(*gc13a2), GFP_KERNEL);
	if (!gc13a2)
		return -ENOMEM;

	gc13a2->dev = dev;

	ret = gc13a2_parse_fwnode(gc13a2);
	if (ret)
		return ret;

	gc13a2->regmap = devm_cci_regmap_init_i2c(client, 16);
	if (IS_ERR(gc13a2->regmap))
		return dev_err_probe(dev, PTR_ERR(gc13a2->regmap),
				     "failed to init CCI\n");

	gc13a2->xclk = devm_v4l2_sensor_clk_get_legacy(dev, NULL, true,
						       GC13A2_DEFAULT_CLK_FREQ);
	if (IS_ERR(gc13a2->xclk))
		return dev_err_probe(dev, PTR_ERR(gc13a2->xclk),
				     "failed to get xclk\n");

	ret = gc13a2_get_regulators(dev, gc13a2);
	if (ret < 0)
		return dev_err_probe(dev, ret, "failed to get regulators\n");

	gc13a2->reset_gpio = devm_gpiod_get(dev, "reset", GPIOD_OUT_HIGH);
	if (IS_ERR(gc13a2->reset_gpio))
		return dev_err_probe(dev, PTR_ERR(gc13a2->reset_gpio),
				     "failed to get reset gpio\n");

	v4l2_i2c_subdev_init(&gc13a2->sd, client, &gc13a2_subdev_ops);
	gc13a2->sd.internal_ops = &gc13a2_internal_ops;
	gc13a2->cur_mode = &gc13a2_modes[0];

	ret = gc13a2_power_on(gc13a2->dev);
	if (ret)
		return dev_err_probe(dev, ret, "failed to power on sensor\n");

	ret = gc13a2_identify_module(gc13a2);
	if (ret)
		goto err_power_off;

	ret = gc13a2_init_controls(gc13a2);
	if (ret) {
		dev_err(dev, "failed to init controls: %d\n", ret);
		goto err_power_off;
	}

	gc13a2->sd.flags |= V4L2_SUBDEV_FL_HAS_DEVNODE;
	gc13a2->pad.flags = MEDIA_PAD_FL_SOURCE;
	gc13a2->sd.dev = dev;
	gc13a2->sd.entity.function = MEDIA_ENT_F_CAM_SENSOR;

	ret = media_entity_pads_init(&gc13a2->sd.entity, 1, &gc13a2->pad);
	if (ret < 0) {
		dev_err(dev, "could not register media entity\n");
		goto err_v4l2_ctrl_handler_free;
	}

	gc13a2->sd.state_lock = gc13a2->ctrls.lock;
	ret = v4l2_subdev_init_finalize(&gc13a2->sd);
	if (ret < 0) {
		dev_err(dev, "v4l2 subdev init error: %d\n", ret);
		goto err_media_entity_cleanup;
	}

	pm_runtime_set_active(gc13a2->dev);
	pm_runtime_enable(gc13a2->dev);
	pm_runtime_set_autosuspend_delay(gc13a2->dev, 1000);
	pm_runtime_use_autosuspend(gc13a2->dev);
	pm_runtime_idle(gc13a2->dev);

	ret = v4l2_async_register_subdev_sensor(&gc13a2->sd);
	if (ret < 0) {
		dev_err(dev, "could not register v4l2 subdev\n");
		goto err_rpm;
	}

	return 0;

err_rpm:
	pm_runtime_disable(gc13a2->dev);
	v4l2_subdev_cleanup(&gc13a2->sd);

err_media_entity_cleanup:
	media_entity_cleanup(&gc13a2->sd.entity);

err_v4l2_ctrl_handler_free:
	v4l2_ctrl_handler_free(gc13a2->sd.ctrl_handler);

err_power_off:
	gc13a2_power_off(gc13a2->dev);

	return ret;
}

static void gc13a2_remove(struct i2c_client *client)
{
	struct v4l2_subdev *sd = i2c_get_clientdata(client);
	struct gc13a2 *gc13a2 = to_gc13a2(sd);

	v4l2_async_unregister_subdev(&gc13a2->sd);
	v4l2_subdev_cleanup(sd);
	media_entity_cleanup(&gc13a2->sd.entity);
	v4l2_ctrl_handler_free(&gc13a2->ctrls);

	pm_runtime_disable(&client->dev);
	if (!pm_runtime_status_suspended(&client->dev))
		gc13a2_power_off(gc13a2->dev);
	pm_runtime_set_suspended(&client->dev);
}

static const struct of_device_id gc13a2_of_match[] = {
	{ .compatible = "galaxycore,gc13a2" },
	{}
};
MODULE_DEVICE_TABLE(of, gc13a2_of_match);

static DEFINE_RUNTIME_DEV_PM_OPS(gc13a2_pm_ops,
				 gc13a2_power_off,
				 gc13a2_power_on,
				 NULL);

static struct i2c_driver gc13a2_i2c_driver = {
	.driver = {
		.of_match_table = gc13a2_of_match,
		.pm = pm_ptr(&gc13a2_pm_ops),
		.name = "gc13a2",
	},
	.probe = gc13a2_probe,
	.remove = gc13a2_remove,
};
module_i2c_driver(gc13a2_i2c_driver);

MODULE_DESCRIPTION("GalaxyCore GC13A2 camera sensor driver");
MODULE_AUTHOR("TB520FU mainline port");
MODULE_LICENSE("GPL");
