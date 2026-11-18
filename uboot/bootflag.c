// SPDX-License-Identifier: GPL-2.0+
/*
 * One-shot boot selection flag for the TB520FU boot menu.
 *
 * Linux asks for "Android on the next boot" by writing a magic value into a
 * spare byte of the PMK8550 SDAM2 block (SPMI peripheral 0x71). The byte keeps
 * its value across a reboot. The menu cannot read a file instead, because
 * U-Boot leaves UFS alone on this tablet.
 *
 * The reset reason byte (0x7148) cannot carry the flag: ABL reads it and then
 * clears it before boot_a runs.
 *
 * "take" succeeds only when the flag held the wanted value AND was cleared AND
 * read back as zero. A flag that cannot be cleared is ignored, so a failed
 * write can never turn into "Android on every boot".
 */

#include <command.h>
#include <dm.h>
#include <stdio.h>
#include <vsprintf.h>
#include <power/pmic.h>

#define BOOTFLAG_PMIC		"pmic@0"	/* PMK8550, SPMI SID 0 */
#define SDAM2_MEM_FIRST		0x7140
#define SDAM2_MEM_LAST		0x71bf
#define SDAM2_RESET_REASON	0x7148		/* owned by ABL: bits [7:1] = mode */
#define RESET_REASON_RECOVERY	0x01
#define RESET_REASON_BOOTLOADER	0x02
#define BOOTFLAG_TEST_VALUE	0x5a		/* never a real request */

static int bootflag_open(const char *arg, struct udevice **devp, uint *regp)
{
	uint reg = hextoul(arg, NULL);
	int ret;

	if (reg < SDAM2_MEM_FIRST || reg > SDAM2_MEM_LAST ||
	    reg == SDAM2_RESET_REASON) {
		printf("bootflag: %#x is not a usable SDAM2 byte\n", reg);
		return -EINVAL;
	}

	ret = pmic_get(BOOTFLAG_PMIC, devp);
	if (ret) {
		printf("bootflag: no %s (%d)\n", BOOTFLAG_PMIC, ret);
		return ret;
	}

	*regp = reg;
	return 0;
}

/* Write a byte and confirm it by reading it back. */
static int bootflag_set(struct udevice *dev, uint reg, uint value)
{
	int ret;

	ret = pmic_reg_write(dev, reg, value);
	if (ret) {
		printf("bootflag: write %#x to %#x failed (%d)\n", value, reg, ret);
		return ret;
	}

	ret = pmic_reg_read(dev, reg);
	if (ret != value) {
		printf("bootflag: %#x reads %d after writing %#x\n", reg, ret, value);
		return -EIO;
	}

	return 0;
}

static int do_reboot_mode(struct cmd_tbl *cmdtp, const char *mode)
{
	struct udevice *dev;
	uint reason, next;
	int val, ret;

	if (!strcmp(mode, "bootloader") || !strcmp(mode, "fastboot"))
		reason = RESET_REASON_BOOTLOADER;
	else if (!strcmp(mode, "recovery"))
		reason = RESET_REASON_RECOVERY;
	else
		return CMD_RET_USAGE;

	ret = pmic_get(BOOTFLAG_PMIC, &dev);
	if (ret) {
		printf("bootflag: no %s (%d)\n", BOOTFLAG_PMIC, ret);
		return CMD_RET_FAILURE;
	}

	val = pmic_reg_read(dev, SDAM2_RESET_REASON);
	if (val < 0) {
		printf("bootflag: read %#x failed (%d)\n",
		       SDAM2_RESET_REASON, val);
		return CMD_RET_FAILURE;
	}

	next = ((uint)val & 0x01) | (reason << 1);
	if (bootflag_set(dev, SDAM2_RESET_REASON, next)) {
		printf("bootflag: reboot-mode %s ignored, write failed\n", mode);
		return CMD_RET_FAILURE;
	}

	printf("bootflag: rebooting to %s (%#x -> %#x)\n",
	       mode, SDAM2_RESET_REASON, next);
	do_reset(cmdtp, 0, 0, NULL);
	return CMD_RET_FAILURE;
}

static int do_bootflag(struct cmd_tbl *cmdtp, int flag, int argc, char *const argv[])
{
	struct udevice *dev;
	uint reg, want;
	int val, ret;

	if (argc < 3)
		return CMD_RET_USAGE;

	if (!strcmp(argv[1], "reboot-mode")) {
		if (argc != 3)
			return CMD_RET_USAGE;
		return do_reboot_mode(cmdtp, argv[2]);
	}

	if (bootflag_open(argv[2], &dev, &reg))
		return CMD_RET_FAILURE;

	val = pmic_reg_read(dev, reg);
	if (val < 0) {
		printf("bootflag: read %#x failed (%d)\n", reg, val);
		return CMD_RET_FAILURE;
	}

	if (!strcmp(argv[1], "get")) {
		printf("bootflag: %#x = %#x\n", reg, val);
		return CMD_RET_SUCCESS;
	}

	if (!strcmp(argv[1], "take")) {
		if (argc != 4)
			return CMD_RET_USAGE;
		want = hextoul(argv[3], NULL);
		if (val != want)
			return CMD_RET_FAILURE;
		if (bootflag_set(dev, reg, 0)) {
			printf("bootflag: request ignored, flag not cleared\n");
			return CMD_RET_FAILURE;
		}
		return CMD_RET_SUCCESS;
	}

	if (!strcmp(argv[1], "test")) {
		/*
		 * Exercises the write path by hand. It never leaves a real
		 * request behind, so a hang here costs one boot, not a loop.
		 */
		printf("bootflag: %#x = %#x before the test\n", reg, val);
		ret = bootflag_set(dev, reg, BOOTFLAG_TEST_VALUE);
		if (!ret)
			ret = bootflag_set(dev, reg, val);
		printf("bootflag: write test %s\n", ret ? "FAILED" : "passed");
		return ret ? CMD_RET_FAILURE : CMD_RET_SUCCESS;
	}

	return CMD_RET_USAGE;
}

U_BOOT_CMD(bootflag, 4, 0, do_bootflag,
	   "one-shot boot selection flag and reboot-mode in PMK8550 SDAM2",
	   "get <reg>          - print the flag byte\n"
	   "bootflag take <reg> <value> - succeed if the byte held <value> and was cleared\n"
	   "bootflag test <reg>         - write a test value, read it back, restore\n"
	   "bootflag reboot-mode <bootloader|recovery> - set ABL reset reason and reset");
