#!/usr/bin/env bash
# SPDX-FileCopyrightText: (c) 2026 Joel Winarske
# SPDX-License-Identifier: MIT
#
# A vkms device with two CRTCs, built through configfs.
#
#   sudo validation/vkms-two-crtc.sh up      # create and enable it
#   sudo validation/vkms-two-crtc.sh down    # disable and remove it
#
# The module-parameter device has one CRTC, so nothing on it can move a scene
# between pipes. This one has two, each with its own primary, encoder and
# connected connector, plus overlays either CRTC may use -- a plane shared
# between pipes is what a rebind to the other CRTC can reuse, and reusing one
# is where the kernel draws its line ("switching CRTC directly").
#
# Needs a kernel with vkms configfs (6.19+). Root, because configfs is.
#
# A desktop session treats the new card as a hotplugged GPU: GNOME's mutter
# opens it, takes DRM master and extends the desktop onto its outputs, and
# every test then fails as not-master. `up` therefore installs a udev rule
# tagging this device -- matched by its path, so nothing else -- with
# mutter-device-ignore before enabling it. `down` leaves the rule in place;
# it matches nothing once the device is gone.
set -euo pipefail

name="${VKMS_DEVICE:-drmkit-two-crtc}"
root=/sys/kernel/config/vkms
dev="$root/$name"
overlays="${VKMS_OVERLAYS:-4}"

rule=/etc/udev/rules.d/61-drmkit-vkms-ignore.rules

up() {
  modprobe vkms
  if [[ ! -e "$rule" ]]; then
    echo "SUBSYSTEM==\"drm\", DEVPATH==\"/devices/faux/$name/*\", TAG+=\"mutter-device-ignore\"" > "$rule"
    udevadm control --reload
  fi
  [[ -d "$root" ]] || { echo "no $root: this kernel's vkms has no configfs" >&2; exit 1; }
  [[ -e "$dev" ]] && { echo "$dev already exists; run 'down' first" >&2; exit 1; }
  mkdir "$dev"

  for i in 0 1; do
    mkdir "$dev/crtcs/crtc$i"
    mkdir "$dev/encoders/encoder$i"
    ln -s "$dev/crtcs/crtc$i" "$dev/encoders/encoder$i/possible_crtcs/"
    mkdir "$dev/connectors/connector$i"
    ln -s "$dev/encoders/encoder$i" "$dev/connectors/connector$i/possible_encoders/"
    # Primaries are per-pipe, as on most hardware.
    mkdir "$dev/planes/primary$i"
    echo 1 > "$dev/planes/primary$i/type"
    ln -s "$dev/crtcs/crtc$i" "$dev/planes/primary$i/possible_crtcs/"
  done

  # Overlays either pipe may use.
  for ((i = 0; i < overlays; i++)); do
    mkdir "$dev/planes/overlay$i"
    echo 0 > "$dev/planes/overlay$i/type"
    for c in 0 1; do
      ln -s "$dev/crtcs/crtc$c" "$dev/planes/overlay$i/possible_crtcs/"
    done
  done

  echo 1 > "$dev/enabled"
  for card in /sys/class/drm/card*; do
    [[ -e "$card/device/driver" ]] || continue
    if [[ "$(basename "$(readlink -f "$card/device")")" == "$name" ]]; then
      echo "/dev/dri/$(basename "$card")"
      return
    fi
  done
  echo "enabled; find the new node under /dev/dri" >&2
}

down() {
  [[ -e "$dev" ]] || return 0
  echo 0 > "$dev/enabled"
  # configfs removes bottom-up: links, then the groups that held them.
  find "$dev" -mindepth 2 -type l -delete
  for kind in connectors encoders planes crtcs; do
    for item in "$dev/$kind"/*; do
      [[ -d "$item" ]] && rmdir "$item"
    done
  done
  rmdir "$dev"
}

case "${1:-}" in
  up) up ;;
  down) down ;;
  *) echo "usage: $0 up|down" >&2; exit 2 ;;
esac
