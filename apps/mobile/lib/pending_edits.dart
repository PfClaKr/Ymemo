/// The editor on screen, if any, and a way to write what is in it right now.
///
/// The editor saves on the way back, and that was the only time it saved. Leaving the app
/// with a memo open never goes back: with "lock when leaving the app" on — the default — the
/// vault was closed and the editor popped from under it, and without it Android may kill the
/// process in the background. Either way what had been typed since the editor opened was
/// gone, and a new memo left an empty row behind. Reproduced on the emulator: type into a new
/// memo, press home, come back — no text, one blank memo.
///
/// So the editor registers here while it is open, and whoever is about to take the vault
/// away (`YmemoApp` closing it, the app going to the background) calls [flush] first.
library;

class PendingEdits {
  PendingEdits._();

  static Future<void> Function()? _flush;

  /// The open editor's "write it now". One editor is open at a time.
  static void attach(Future<void> Function() flush) => _flush = flush;

  /// Called by the editor on its way out; a newer editor's hook is left alone.
  static void detach(Future<void> Function() flush) {
    if (_flush == flush) _flush = null;
  }

  /// Writes whatever the open editor holds. Never throws: it runs on the way to closing the
  /// vault, and a failure here must not keep the vault open.
  static Future<void> flush() async {
    final flush = _flush;
    if (flush == null) return;
    try {
      await flush();
    } catch (_) {
      // The editor reports its own failures; there is nothing more to do here.
    }
  }
}
