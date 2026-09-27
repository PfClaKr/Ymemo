/// The app's two themes, and the one screen family that keeps the light one regardless.
///
/// The screens around the notes — the list, settings, pairing, the lock screen — follow the
/// phone's dark mode. A note does not: its editor and its history are drawn on the note's own
/// paper (see `palette.dart`), which is light by what it is, so they are always set in the
/// light theme ([PaperTheme]) and their text, caret and underlines stay dark on it.
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

/// The app's accent, the gold of the icon.
const _seed = Color(0xFFE6D24A);

/// Status bar icons that read on a background of [brightness]. Transparent, because the app
/// draws edge to edge; set through every AppBar, and globally for screens without one.
SystemUiOverlayStyle overlayFor(Brightness brightness) => SystemUiOverlayStyle(
      statusBarColor: Colors.transparent,
      statusBarIconBrightness:
          brightness == Brightness.dark ? Brightness.light : Brightness.dark,
      statusBarBrightness: brightness,
    );

ThemeData ymemoTheme(Brightness brightness) => ThemeData(
      colorScheme: ColorScheme.fromSeed(seedColor: _seed, brightness: brightness),
      useMaterial3: true,
      appBarTheme: AppBarTheme(systemOverlayStyle: overlayFor(brightness)),
    );

/// A note's own screens: always the light theme, whatever the phone is set to.
///
/// Sheets and dialogs opened from inside inherit it too — `showModalBottomSheet` and
/// `showDialog` capture the theme of the context they are called from.
class PaperTheme extends StatelessWidget {
  const PaperTheme({super.key, required this.builder});

  final WidgetBuilder builder;

  @override
  Widget build(BuildContext context) =>
      Theme(data: ymemoTheme(Brightness.light), child: Builder(builder: builder));
}
