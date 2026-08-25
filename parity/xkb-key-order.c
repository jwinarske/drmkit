// SPDX-FileCopyrightText: (c) 2026 Joel Winarske
// SPDX-License-Identifier: MIT
//
// Which order drm-cxx's Keyboard::process_key resolves a key in, and what it
// costs. Build:
//
//   cc -o xkb-key-order parity/xkb-key-order.c $(pkg-config --cflags --libs xkbcommon)
//
// src/input/keyboard.cpp:281-300 updates the xkb state and *then* reads the
// keysym off it. This runs both orders over the same keymap and prints what
// each types.

#include <stdio.h>
#include <string.h>
#include <xkbcommon/xkbcommon.h>

static const char *KEYMAP =
    "xkb_keymap {\n"
    "  xkb_keycodes { minimum = 8; maximum = 255;\n"
    "    <LFSH> = 50; <AC01> = 38; };\n"
    "  xkb_types { include \"basic\" };\n"
    "  xkb_compat {\n"
    "    interpret Shift_L { action = LatchMods(modifiers = Shift); };\n"
    "  };\n"
    "  xkb_symbols {\n"
    "    key <LFSH> { [ Shift_L ] };\n"
    "    key <AC01> { type = \"ALPHABETIC\", [ a, A ] };\n"
    "    modifier_map Shift { <LFSH> };\n"
    "  };\n"
    "};\n";

// evdev + 8.
#define KC_SHIFT 50
#define KC_A 38
#define KC_APOS 48

// Arm the latch: press and release Shift.
static void latch_shift(struct xkb_state *state) {
  xkb_state_update_key(state, KC_SHIFT, XKB_KEY_DOWN);
  xkb_state_update_key(state, KC_SHIFT, XKB_KEY_UP);
}

int main(void) {
  struct xkb_context *ctx = xkb_context_new(XKB_CONTEXT_NO_FLAGS);
  struct xkb_keymap *keymap = xkb_keymap_new_from_string(
      ctx, KEYMAP, XKB_KEYMAP_FORMAT_TEXT_V1, XKB_KEYMAP_COMPILE_NO_FLAGS);
  if (keymap == NULL) {
    fprintf(stderr, "keymap failed to compile\n");
    return 1;
  }

  char buf[8];

  // drm-cxx's order: update, then read.
  struct xkb_state *after = xkb_state_new(keymap);
  latch_shift(after);
  xkb_state_update_key(after, KC_A, XKB_KEY_DOWN);
  memset(buf, 0, sizeof(buf));
  xkb_state_key_get_utf8(after, KC_A, buf, sizeof(buf));
  printf("update then read: \"%s\"\n", buf);

  // What xkbcommon documents: read, then update.
  struct xkb_state *before = xkb_state_new(keymap);
  latch_shift(before);
  memset(buf, 0, sizeof(buf));
  xkb_state_key_get_utf8(before, KC_A, buf, sizeof(buf));
  xkb_state_update_key(before, KC_A, XKB_KEY_DOWN);
  printf("read then update: \"%s\"\n", buf);

  xkb_state_unref(after);
  xkb_state_unref(before);
  xkb_keymap_unref(keymap);

  // The same thing on a layout people actually have installed. Latvian types
  // its long vowels by latching level 3 with the apostrophe key: ' then a is
  // how you write a-macron.
  struct xkb_rule_names names = {NULL, NULL, "lv", "apostrophe", NULL};
  struct xkb_keymap *lv = xkb_keymap_new_from_names(ctx, &names, 0);
  if (lv == NULL) {
    fprintf(stderr, "lv(apostrophe) is not installed; skipping\n");
    xkb_context_unref(ctx);
    return 0;
  }
  for (int update_first = 1; update_first >= 0; update_first--) {
    struct xkb_state *state = xkb_state_new(lv);
    xkb_state_update_key(state, KC_APOS, XKB_KEY_DOWN);
    xkb_state_update_key(state, KC_APOS, XKB_KEY_UP);
    memset(buf, 0, sizeof(buf));
    if (update_first) {
      xkb_state_update_key(state, KC_A, XKB_KEY_DOWN);
      xkb_state_key_get_utf8(state, KC_A, buf, sizeof(buf));
    } else {
      xkb_state_key_get_utf8(state, KC_A, buf, sizeof(buf));
      xkb_state_update_key(state, KC_A, XKB_KEY_DOWN);
    }
    printf("lv(apostrophe), %s: \"%s\"\n",
           update_first ? "update then read" : "read then update", buf);
    xkb_state_unref(state);
  }
  xkb_keymap_unref(lv);
  xkb_context_unref(ctx);
  return 0;
}
