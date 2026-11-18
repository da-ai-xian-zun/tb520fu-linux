#!/bin/bash
# SPDX-License-Identifier: MIT
# Sample running Firefox: per-process-type CPU, iris IRQ rate, battery power. usage: ffsample.sh [secs]
S=${1:-10}; B=/sys/class/power_supply/qcom-battmgr-bat
irq(){ grep -i iris /proc/interrupts | awk '{s=0; for(i=2;i<=9;i++) s+=$i; print s}'; }
snap(){ for p in $(pgrep -f firefox-esr); do
  t=$(sed 's/.*) //' /proc/$p/stat 2>/dev/null | awk '{print $12+$13}'); [ -n "$t" ] || continue
  k=$(tr '\0' '\n' </proc/$p/cmdline | awk '{w=$NF} END{print w}'); case $k in rdd|tab|gpu|socket|utility|forkserver) ;; *) k=parent;; esac
  echo "$p $k $t"; done; }
cpuall(){ awk '/^cpu /{print $2+$3+$4+$7+$8}' /proc/stat; }
a=$(snap); i1=$(irq); c1=$(cpuall); pw=0
for i in $(seq $S); do sleep 1; pw=$(( pw + $(cat $B/current_now | tr -d -) * $(cat $B/voltage_now) / 1000000000 )); done
b=$(snap); i2=$(irq); c2=$(cpuall)
join <(echo "$a"|sort) <(echo "$b"|sort) | awk -v S=$S '{d=$5-$3; s[$2]+=d; t+=d} END{for(k in s) if(s[k]>0) printf "%s=%.0f%% ", k, s[k]/S; printf "| firefox=%.0f%%", t/S}'
echo " | system=$(( (c2-c1)/S ))% | iris irq/s=$(( (i2-i1)/S )) | battery=$((pw/S))mW $(cat $B/status)"
