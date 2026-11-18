#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""把 TB520FU_PERF=1 每秒打一次的 perf: 行汇总成一张表（scripts/perf-run.sh 的输出直接管道进来）。

列含义：n 样本数；avg/max 毫秒；>8.3 / >16.7 超过一个 120Hz / 60Hz 刷新周期的样本数。
gauge 类（instances[id]、overdraw%[id]）只给平均和最大值。非 perf: 行原样打印。
"""
import collections
import re
import sys

spans = collections.defaultdict(lambda: [0, 0.0, 0.0, 0, 0])
gauges = collections.defaultdict(lambda: [0, 0, 0])
for line in sys.stdin:
    m = re.match(r'perf:\s+(\S+)\s+n=(\d+)\s+avg=([\d.]+)ms max=([\d.]+)ms\s+>8.3ms:(\d+) >16.7ms:(\d+)', line)
    g = re.match(r'perf:\s+(\S+)\s+n=(\d+)\s+avg=(\d+) max=(\d+)\s*$', line)
    if m:
        a = spans[m.group(1)]
        n = int(m.group(2))
        a[0] += n
        a[1] += n * float(m.group(3))
        a[2] = max(a[2], float(m.group(4)))
        a[3] += int(m.group(5))
        a[4] += int(m.group(6))
    elif g:
        a = gauges[g.group(1)]
        n = int(g.group(2))
        a[0] += n
        a[1] += n * int(g.group(3))
        a[2] = max(a[2], int(g.group(4)))
    elif not line.startswith('perf:'):
        print(line.rstrip())

print('%-22s %5s %8s %8s %7s %7s' % ('span', 'n', 'avg ms', 'max ms', '>8.3', '>16.7'))
for k, a in sorted(spans.items()):
    print('%-22s %5d %8.1f %8.1f %7d %7d' % (k, a[0], a[1] / a[0], a[2], a[3], a[4]))
for k, a in sorted(gauges.items()):
    print('%-22s %5d   avg=%d max=%d' % (k, a[0], a[1] // a[0], a[2]))
