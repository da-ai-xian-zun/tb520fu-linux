// SPDX-License-Identifier: GPL-2.0-only
/*
 * Lenovo Yoga Pad Pro 12.7 (TB520FU, "Lapis") QSH Sensor PMIC Rail Enabler
 *
 * Ensures PM8550 LDO rails required by ADSP QSH sensors (LSM6DSO SPI IMU and
 * TCS3408 ALS) match Android's always-on / active-only RPMh VRM votes:
 *   - ldob1  (pm_humu_l1):  1800 mV HPM (Android regulator-always-on, sensor_vddio)
 *   - ldob2  (pm_humu_l2):  3008 mV HPM (ADSP sensor_vdd)
 *   - ldob9  (pm_humu_l9):  3000 mV HPM (Android peripheral 3.0V rail)
 *   - ldob14 (pm_humu_l14): 3200 mV HPM (Android regulator-always-on 3.2V rail)
 *
 * Votes are issued in RPMH_ACTIVE_ONLY_STATE so APSS automatically drops its
 * active vote during deep system suspend (s2ram).
 */

#include <linux/err.h>
#include <linux/init.h>
#include <linux/kernel.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/of_platform.h>
#include <linux/platform_device.h>
#include <soc/qcom/cmd-db.h>
#include <soc/qcom/rpmh.h>

#define VRM_VOLTAGE_OFFSET	0x0
#define VRM_ENABLE_OFFSET	0x4
#define VRM_MODE_OFFSET		0x8
#define PMIC5_LDO_MODE_HPM	7

struct tb520fu_rail_cfg {
	const char *rsc_name;
	u32 mv;
};

static const struct tb520fu_rail_cfg tb520fu_rails[] = {
	{ "ldob1",  1800 },
	{ "ldob2",  3008 },
	{ "ldob9",  3000 },
	{ "ldob14", 3200 },
};

static int tb520fu_vote_rail(struct device *rsc_dev,
			     const struct tb520fu_rail_cfg *rail)
{
	struct tcs_cmd cmd = { .wait = true };
	u32 addr;
	int ret;

	addr = cmd_db_read_addr(rail->rsc_name);
	if (!addr) {
		pr_err("tb520fu_sensor_rails: %s not found in cmd-db\n",
		       rail->rsc_name);
		return -ENODEV;
	}

	cmd.addr = addr + VRM_VOLTAGE_OFFSET;
	cmd.data = rail->mv;
	ret = rpmh_write(rsc_dev, RPMH_ACTIVE_ONLY_STATE, &cmd, 1);
	if (ret) {
		pr_err("tb520fu_sensor_rails: %s set %u mV failed: %d\n",
		       rail->rsc_name, rail->mv, ret);
		return ret;
	}

	cmd.addr = addr + VRM_MODE_OFFSET;
	cmd.data = PMIC5_LDO_MODE_HPM;
	ret = rpmh_write(rsc_dev, RPMH_ACTIVE_ONLY_STATE, &cmd, 1);
	if (ret) {
		pr_err("tb520fu_sensor_rails: %s set HPM failed: %d\n",
		       rail->rsc_name, ret);
		return ret;
	}

	cmd.addr = addr + VRM_ENABLE_OFFSET;
	cmd.data = 1;
	ret = rpmh_write(rsc_dev, RPMH_ACTIVE_ONLY_STATE, &cmd, 1);
	if (ret) {
		pr_err("tb520fu_sensor_rails: %s enable failed: %d\n",
		       rail->rsc_name, ret);
		return ret;
	}

	pr_info("tb520fu_sensor_rails: %s (0x%x) enabled at %u mV HPM (ACTIVE_ONLY)\n",
		rail->rsc_name, addr, rail->mv);
	return 0;
}

static int __init tb520fu_sensor_rails_init(void)
{
	struct device_node *np;
	struct platform_device *pdev;
	int i, ret = 0;

	np = of_find_compatible_node(NULL, NULL, "qcom,pm8550-rpmh-regulators");
	if (!np) {
		pr_err("tb520fu_sensor_rails: pm8550-rpmh-regulators node not found\n");
		return -ENODEV;
	}

	pdev = of_find_device_by_node(np);
	of_node_put(np);
	if (!pdev) {
		pr_err("tb520fu_sensor_rails: pm8550-rpmh-regulators pdev not ready\n");
		return -EPROBE_DEFER;
	}

	for (i = 0; i < ARRAY_SIZE(tb520fu_rails); i++) {
		int r = tb520fu_vote_rail(&pdev->dev, &tb520fu_rails[i]);
		if (r && !ret)
			ret = r;
	}

	put_device(&pdev->dev);
	return ret;
}

static void __exit tb520fu_sensor_rails_exit(void)
{
	pr_info("tb520fu_sensor_rails: unloaded\n");
}

module_init(tb520fu_sensor_rails_init);
module_exit(tb520fu_sensor_rails_exit);

MODULE_DESCRIPTION("Lenovo TB520FU QSH Sensor PMIC Rail Enabler");
MODULE_LICENSE("GPL");
