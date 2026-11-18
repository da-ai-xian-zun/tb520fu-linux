#!/bin/sh
# SPDX-License-Identifier: MIT
# Rebuild Debian forky's bluez source package on trixie (tablet, native arm64). Log: /root/bluez-backport.log
# Result debs in /root/bluez-bpo/. Does NOT install them.
set -x
exec > /root/bluez-backport.log 2>&1
export DEBIAN_FRONTEND=noninteractive
M=http://mirrors.ustc.edu.cn/debian
echo "deb-src $M forky main" > /etc/apt/sources.list.d/forky-src.list
apt-get update || exit 11
apt-get install -y --no-install-recommends build-essential devscripts fakeroot dpkg-dev || exit 12
mkdir -p /root/bluez-bpo && cd /root/bluez-bpo || exit 13
rm -rf bluez-5.*/
apt-get source bluez/forky || exit 14
cd bluez-5.*/ || exit 15
dpkg-checkbuilddeps 2>&1
apt-get build-dep -y --no-install-recommends bluez/forky || { echo BUILD-DEP-FAILED; mk-build-deps -i -r -t "apt-get -y --no-install-recommends" debian/control || exit 16; }
DEB_BUILD_OPTIONS="nocheck parallel=8" dpkg-buildpackage -b -uc -us || exit 17
ls -la /root/bluez-bpo/*.deb
echo BACKPORT-DONE
