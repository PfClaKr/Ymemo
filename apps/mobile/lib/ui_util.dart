/// Small layout helpers shared by the screens.
library;

import 'package:flutter/material.dart';

/// Height of the system navigation bar (or gesture pill) at the bottom of the screen.
///
/// Scrollables add it to their padding rather than being wrapped in a `SafeArea`: the
/// content still scrolls *under* the translucent bar, which is the point of edge to edge,
/// but the last row can be scrolled clear of it instead of ending up underneath.
double bottomInset(BuildContext context) => MediaQuery.paddingOf(context).bottom;
