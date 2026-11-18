// SPDX-License-Identifier: GPL-2.0+
/*
 * Start an arm64 kernel with exactly the devicetree the previous bootloader
 * gave us, unmodified. booti cannot be used for this: it rewrites /chosen and
 * the memory node, and Android depends on what ABL put there.
 */

#include <command.h>
#include <cpu_func.h>
#include <init.h>
#include <irq_func.h>
#include <linux/libfdt.h>
#include <mapmem.h>
#include <stdio.h>
#include <vsprintf.h>
#include <asm/u-boot-arm.h>

#define ARM64_IMAGE_MAGIC_OFFSET	56
#define ARM64_IMAGE_MAGIC		0x644d5241	/* "ARM\x64" */

static int do_androidboot(struct cmd_tbl *cmdtp, int flag, int argc, char *const argv[])
{
	void (*entry)(ulong fdt, ulong x1, ulong x2, ulong x3);
	phys_addr_t fdt = get_prev_bl_fdt_addr();
	ulong addr;

	if (argc != 2)
		return CMD_RET_USAGE;

	addr = hextoul(argv[1], NULL);
	if (addr & 0x1fffff) {
		printf("androidboot: %#lx is not 2 MiB aligned\n", addr);
		return CMD_RET_FAILURE;
	}
	if (*(u32 *)(addr + ARM64_IMAGE_MAGIC_OFFSET) != ARM64_IMAGE_MAGIC) {
		printf("androidboot: no arm64 Image at %#lx\n", addr);
		return CMD_RET_FAILURE;
	}
	if (!fdt || fdt_check_header((void *)fdt)) {
		printf("androidboot: no valid devicetree from ABL (%#llx)\n", (u64)fdt);
		return CMD_RET_FAILURE;
	}

	printf("Starting kernel at %#lx, devicetree %#llx\n", addr, (u64)fdt);

	entry = (void *)addr;
	disable_interrupts();
	cleanup_before_linux();
	entry(fdt, 0, 0, 0);

	return CMD_RET_FAILURE;
}

U_BOOT_CMD(androidboot, 2, 0, do_androidboot,
	   "start an arm64 kernel with the previous bootloader's devicetree",
	   "<address>");
