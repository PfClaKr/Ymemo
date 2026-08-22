// The sticky palette, in the one place it becomes a real color.
//
// This mirrors `crates/ymemo-desktop/ui/theme.slint` deliberately, and the two have to keep
// agreeing: the core stores and syncs **the key string alone** (`"yellow"`, `"pink"`, ...),
// so a memo colored on the phone is the same memo colored on the desktop only as long as
// both sides map that key to the same color. Add a key here and there, or not at all.
//
// The colors themselves are the desktop's, minus the translucency: a sticky floats over a
// desktop and reads through it, while a list row and an editor sit on an opaque screen.

import 'package:flutter/material.dart';

/// Palette keys in the order both UIs offer them; the first is the default.
const stickyColors = ['yellow', 'pink', 'green', 'blue', 'purple'];

/// The key a memo or folder carries when it has never been colored, matching
/// `ymemo_core::DEFAULT_COLOR`. An unknown key falls back to it rather than failing, so a
/// vault written by a newer version still draws.
const defaultColor = 'yellow';

/// Body background: the sticky itself, and the editor screen.
Color paletteBg(String key) => switch (key) {
      'pink' => const Color(0xFFFFD6E6),
      'green' => const Color(0xFFD2F0C8),
      'blue' => const Color(0xFFCFE6FF),
      'purple' => const Color(0xFFE6D6F5),
      _ => const Color(0xFFFFF9B1),
    };

/// Title bar: opaque and darker than the body.
Color paletteBar(String key) => switch (key) {
      'pink' => const Color(0xFFF7B8CE),
      'green' => const Color(0xFFB6E0A8),
      'blue' => const Color(0xFFB0D4F2),
      'purple' => const Color(0xFFD0B8EC),
      _ => const Color(0xFFF4E98C),
    };

/// Title bar text, kept legible on [paletteBar] for each color.
Color paletteInk(String key) => switch (key) {
      'pink' => const Color(0xFF7A3350),
      'green' => const Color(0xFF35662F),
      'blue' => const Color(0xFF2A5578),
      'purple' => const Color(0xFF573377),
      _ => const Color(0xFF5C5C25),
    };

/// Saturated accent, for the swatches you pick from and the stripe on a list row.
Color paletteSwatch(String key) => switch (key) {
      'pink' => const Color(0xFFFF9FC0),
      'green' => const Color(0xFF8FD678),
      'blue' => const Color(0xFF7DB8EC),
      'purple' => const Color(0xFFB98FE0),
      _ => const Color(0xFFFFE15C),
    };

/// Background for one row of the memo list.
///
/// [paletteBg] undiluted turns a scrolling list into five bands of poster paint, so the row
/// gets a fraction of it over the surface color — enough to tell two colors apart at a
/// glance, faint enough to read black text on.
Color paletteRow(String key, Color surface) =>
    Color.alphaBlend(paletteBg(key).withValues(alpha: 0.42), surface);

/// The palette as a row of tappable circles, with the current one ringed.
///
/// Both places that change a color use this, so the folder sheet and the editor cannot drift
/// apart in which colors they offer or how a choice looks.
class ColorSwatches extends StatelessWidget {
  const ColorSwatches({super.key, required this.selected, required this.onPick});

  final String selected;
  final void Function(String) onPick;

  @override
  Widget build(BuildContext context) {
    return Row(
      mainAxisAlignment: MainAxisAlignment.spaceEvenly,
      children: [
        for (final key in stickyColors)
          InkWell(
            onTap: () => onPick(key),
            customBorder: const CircleBorder(),
            child: Padding(
              padding: const EdgeInsets.all(6),
              child: Container(
                width: 34,
                height: 34,
                decoration: BoxDecoration(
                  color: paletteSwatch(key),
                  shape: BoxShape.circle,
                  border: Border.all(
                    color: key == selected ? paletteInk(key) : Colors.black26,
                    width: key == selected ? 3 : 1,
                  ),
                ),
              ),
            ),
          ),
      ],
    );
  }
}
