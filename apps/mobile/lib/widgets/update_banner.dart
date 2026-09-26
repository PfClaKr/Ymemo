/// The banner that says a newer release is out.
library;

import 'package:flutter/material.dart';

import '../host.dart' as host;
import '../src/rust/api.dart';

/// One line saying a newer release exists, above the memo list.
///
/// Never a dialog and never dismissible-by-accident: it is information, and the app it sits
/// on top of is for writing memos, not for updating itself.
class UpdateBanner extends StatelessWidget {
  const UpdateBanner({super.key, required this.strings, required this.release});

  final FfiStrings strings;
  final FfiRelease release;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Material(
      color: scheme.secondaryContainer,
      child: InkWell(
        onTap: () => host.openUrl(release.url),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
          child: Row(
            children: [
              const Icon(Icons.system_update, size: 18),
              const SizedBox(width: 8),
              Expanded(child: Text('${strings.updateAvailable} ${release.version}')),
              Text(
                strings.updateOpen,
                style: TextStyle(color: scheme.primary, fontSize: 12),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
