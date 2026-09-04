// The home-screen widgets' half of the app: what gets published for them to draw, and what
// they ask the app to do when one is tapped.
//
// A widget is drawn by the launcher, in the launcher's process, at moments this app has no
// say in — while it is closed, while it is locked, seconds after a reboot. So it cannot ask
// the vault anything (that needs the master key) and it must not read `ymemo.db` either:
// `Vault::rebuild()` clears that cache and re-materializes it on every merge, and a widget
// reading it mid-rebuild would show an empty list. What it reads instead is the snapshot
// published from here, which is a **copy** of some memo text living outside the vault —
// truncated to a preview, in the app's private directory, never synced, excluded from
// backup, and emptied the moment the vault closes. SECURITY.md says the same thing about
// the plaintext cache it sits next to.
//
// The Android side is `android/app/src/main/kotlin/dev/ymemo/ymemo_mobile/widget/`.

import 'dart:convert';

import 'package:flutter/widgets.dart';

import 'host.dart' as host;
import 'src/rust/api.dart';

/// What a tapped widget, or a launcher shortcut, asked the app to do.
enum WidgetAction { newMemo, newPhotoMemo, openList, openMemo, openFolder }

/// One request, with the memo or folder id for the two actions that name one.
class WidgetRequest {
  const WidgetRequest(this.action, [this.id = '']);

  final WidgetAction action;
  final String id;

  /// The names the Kotlin side sends; they are also the constants in `widget/Launch.kt`.
  static const _actions = {
    'new_memo': WidgetAction.newMemo,
    'new_photo_memo': WidgetAction.newPhotoMemo,
    'open_list': WidgetAction.openList,
    'open_memo': WidgetAction.openMemo,
    'open_folder': WidgetAction.openFolder,
  };

  /// Null for anything unrecognised. A PendingIntent outlives the install that created it,
  /// so an action this version has never heard of is a real possibility and doing nothing
  /// is the right answer to it.
  static WidgetRequest? parse(Map<Object?, Object?>? raw) {
    final action = _actions[raw?['action']];
    if (action == null) return null;
    return WidgetRequest(action, raw?['id'] as String? ?? '');
  }
}

/// The request waiting for a screen that can carry it out.
///
/// It waits here rather than being handled where it arrives because a widget can be tapped
/// while the vault is locked: the request then sits through the password screen instead of
/// being dropped, and the memo opens once there is a vault to open it from.
final pendingWidgetRequest = ValueNotifier<WidgetRequest?>(null);

/// Starts listening for widget taps, and picks up the one that launched the app if there
/// was one. Called once, before anything is drawn.
Future<void> startWidgetRequests() async {
  host.onWidgetAction((raw) => pendingWidgetRequest.value = WidgetRequest.parse(raw));
  final launched = WidgetRequest.parse(await host.takeWidgetAction());
  if (launched != null) pendingWidgetRequest.value = launched;
}

/// How much of a body is published, in code points. Enough to fill a sticky widget at any
/// size a launcher offers, and short enough that the snapshot is not a second copy of the
/// vault.
const _previewLength = 400;

/// How many memos are published. A home screen is not an archive.
const _memoLimit = 100;

/// The last snapshot handed over, so the 15s merge timer does not redraw every widget on
/// the home screen every 15 seconds to show it exactly what it is already showing.
String? _published;

/// Publishes what the widgets draw. Called after anything that changes a memo or folder.
///
/// The list is deliberately not the app's root screen. That screen shows the top level only,
/// which on a home screen would hide every memo ever filed in a folder; a widget is a
/// shortcut surface, so it gets the folders to go into and then every memo, most recently
/// edited first.
Future<void> publishWidgets() async {
  try {
    final folders = await groupChildren(parentId: '');
    final memos = await memoList(); // already most recently updated first
    final name = await vaultName();
    await _publish(jsonEncode({
      'vault': name,
      'hidden': false,
      'folders': [
        for (final folder in folders)
          {'id': folder.id, 'title': folder.name, 'preview': '', 'color': folder.color},
      ],
      'memos': [
        for (final memo in memos.take(_memoLimit))
          {
            'id': memo.id,
            'title': memo.title,
            'preview': _preview(memo.body),
            'color': memo.color,
          },
      ],
    }));
  } catch (e) {
    // The vault closing under a reload is the ordinary way here; a home screen that keeps
    // showing what it showed a second ago is not worth interrupting anyone over.
    debugPrint('could not publish the widget snapshot: $e');
  }
}

/// Empties the widgets, because the vault has been closed.
///
/// Locking that left a page of memo text on the home screen would be no lock at all — the
/// same reasoning as `FLAG_SECURE` on the app-switcher thumbnail. It is unconditional:
/// closing the vault is the only thing that gets here, whether it was the lock button or the
/// app being left with "lock when the app is left" on.
Future<void> hideWidgets() => _publish(jsonEncode({'hidden': true}));

Future<void> _publish(String snapshot) async {
  if (snapshot == _published) return;
  _published = snapshot;
  await host.widgetPublish(snapshot);
}

/// Cut by code point, never mid-character: `substring` would happily halve an emoji.
String _preview(String body) {
  final runes = body.runes.toList();
  if (runes.length <= _previewLength) return body;
  return '${String.fromCharCodes(runes.take(_previewLength))}…';
}
