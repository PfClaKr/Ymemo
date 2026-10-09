/// Settings: language, locking, sync timings, new-memo defaults and the update check.
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../host.dart' as host;
import '../palette.dart';
import '../security.dart';
import '../settings.dart';
import '../src/rust/api.dart';
import '../sync.dart';
import '../ui_util.dart';

/// What the last update check concluded; the text for it is built at draw time.
enum _UpdateState { idle, checking, latest, found, failed }

/// Device-local preferences: language, locking, updates.
///
/// Everything applies as it is changed — a phone settings screen with a save button is a
/// phone settings screen someone will leave without pressing it. Rust sanitizes on write, and
/// what comes back is what is shown, so an impossible value cannot sit here looking accepted.
class SettingsScreen extends StatefulWidget {
  const SettingsScreen({
    super.key,
    required this.strings,
    required this.settings,
    required this.sync,
    required this.vaultDir,
    required this.onLock,
    required this.onLanguageChanged,
  });

  final FfiStrings strings;
  final SettingsStore settings;

  /// Only for the Wi-Fi switch: flipping it has to reach the running daemon now.
  final SyncController sync;

  /// Passed through to the security screen, which asks `vault.json` itself whether a
  /// recovery code exists.
  final String vaultDir;

  final Future<void> Function() onLock;
  final Future<void> Function(String) onLanguageChanged;

  @override
  State<SettingsScreen> createState() => _SettingsScreenState();
}

class _SettingsScreenState extends State<SettingsScreen> {
  /// Offered stay-unlocked periods. A free-text number field would be worse in every way:
  /// harder to tap and able to produce values Rust would only clamp away again.
  static const _dayChoices = [0, 1, 7, 30, 90, 365];

  /// The advanced timings, same reasoning. Each list starts at what the core clamps to and
  /// ends where going further stops being useful.
  static const _mergeChoices = [3, 5, 10, 15, 30, 60, 300];
  static const _watchChoices = [1, 2, 5, 10, 20, 60];
  static const _rescanChoices = [60, 300, 900, 3600];

  /// Retention is in days rather than seconds, and 0 is a real answer: keep nothing.
  static const _keepChoices = [0, 1, 7, 30, 90, 365];

  /// What the last check concluded. Kept as state rather than as a finished sentence: a
  /// rendered string would still be in the old language after the language is changed.
  _UpdateState _updateState = _UpdateState.idle;
  String? _updateError;
  FfiRelease? _update;
  bool _checking = false;

  FfiSettings get _s => widget.settings.value;

  /// The running version, asked once rather than on every redraw.
  late final Future<String> _version = appVersion();

  /// Writes one changed field and redraws with whatever Rust kept.
  Future<void> _save({
    String? lang,
    int? unlockDays,
    bool? lockOnBackground,
    bool? biometricUnlock,
    bool? updateCheck,
    int? mergeSeconds,
    int? watchDelaySeconds,
    int? rescanSeconds,
    int? keepVersionsDays,
    bool? wifiOnlySync,
    String? defaultColor,
  }) async {
    await widget.settings.save(FfiSettings(
      lang: lang ?? _s.lang,
      unlockDays: unlockDays ?? _s.unlockDays,
      lockOnBackground: lockOnBackground ?? _s.lockOnBackground,
      biometricUnlock: biometricUnlock ?? _s.biometricUnlock,
      updateCheck: updateCheck ?? _s.updateCheck,
      mergeSeconds: mergeSeconds ?? _s.mergeSeconds,
      watchDelaySeconds: watchDelaySeconds ?? _s.watchDelaySeconds,
      rescanSeconds: rescanSeconds ?? _s.rescanSeconds,
      keepVersionsDays: keepVersionsDays ?? _s.keepVersionsDays,
      wifiOnlySync: wifiOnlySync ?? _s.wifiOnlySync,
      lastUpdateCheck: _s.lastUpdateCheck,
      defaultColor: defaultColor ?? _s.defaultColor,
    ));
    // Flipping the switch has to take effect now, not at the next daemon start.
    if (wifiOnlySync != null) {
      await widget.sync.applyNetworkPolicy();
    }
    // The watch delay is Syncthing's, not ours, so saving has to push it across. It is a
    // no-op while the daemon is down; sync.dart applies it again when it comes up.
    if (watchDelaySeconds != null || rescanSeconds != null) {
      try {
        await syncSetTiming(
          watchDelaySeconds: _s.watchDelaySeconds,
          rescanSeconds: _s.rescanSeconds,
        );
      } catch (e) {
        debugPrint('could not apply the sync timing: $e');
      }
    }
    if (keepVersionsDays != null) {
      try {
        await syncSetVersioning(keepDays: _s.keepVersionsDays);
      } catch (e) {
        debugPrint('could not apply the version retention: $e');
      }
    }
    // One switch, two protections: closing the vault and keeping the memos out of the app
    // switcher. Someone who turned it off chose convenience, and hiding their thumbnail
    // anyway would be deciding for them.
    if (lockOnBackground != null) {
      await host.setScreenshotBlock(lockOnBackground);
    }
    if (!mounted) return;
    setState(() {});
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text(widget.strings.saved), duration: const Duration(seconds: 1)),
    );
  }

  Future<void> _setLanguage(String lang) async {
    await _save(lang: lang);
    await widget.onLanguageChanged(lang);
  }

  /// Shortening the stay-unlocked window has to invalidate the key already stored under the
  /// old one, or the setting would be a suggestion rather than a rule.
  Future<void> _setUnlockDays(int days) async {
    await _save(unlockDays: days);
    await const SessionStore().clear();
  }

  /// Turns biometric unlock on or off.
  ///
  /// Turning it **on** is the moment the key is stored, and it can only happen from here:
  /// this screen is inside the unlocked app, so there is a key to store. The fingerprint is
  /// checked first — not for security, since the vault is already open, but so that a switch
  /// that cannot work never ends up looking on.
  ///
  /// Turning it **off** deletes the key, which is the entire promise of the switch.
  Future<void> _setBiometric(bool on) async {
    const store = BiometricStore();
    if (!on) {
      await store.disable();
      await _save(biometricUnlock: false);
      return;
    }
    if (!await store.available) {
      if (mounted) _say(widget.strings.biometricUnavailable);
      return;
    }
    if (!await store.confirm(
      widget.strings.biometricUnlock,
      title: widget.strings.biometricPrompt,
      cancel: widget.strings.cancel,
    )) {
      if (mounted) _say(widget.strings.biometricFailed);
      return;
    }
    try {
      await store.enable(await vaultKey());
    } catch (e) {
      debugPrint('could not store the biometric key: $e');
      if (mounted) _say('$e');
      return;
    }
    await _save(biometricUnlock: true);
  }

  /// One "N seconds" dropdown. A value outside the offered list — a hand-edited
  /// settings.json, or a list that shrank between versions — still shows, rather than
  /// snapping to something the user never chose.
  Widget _seconds(String label, String hint, List<int> choices, int value,
      void Function(int) onPick) {
    // A growable copy, always: `..sort()` binds to the whole conditional, so returning the
    // const list on the common path and sorting it threw "cannot modify an unmodifiable list".
    final items = [...choices];
    if (!items.contains(value)) {
      items.add(value);
      items.sort();
    }
    return ListTile(
      title: Text(label),
      subtitle: Text(hint),
      trailing: DropdownButton<int>(
        value: value,
        onChanged: (v) => v == null ? null : onPick(v),
        items: [
          for (final n in items)
            DropdownMenuItem(value: n, child: Text('$n ${widget.strings.secondsUnit}')),
        ],
      ),
    );
  }

  /// Shows the tail of the problem log, with one button that puts it on the clipboard —
  /// which is the whole point: a bug report someone can paste rather than describe.
  Future<void> _showLog() async {
    final s = widget.strings;
    final text = await diagTail(maxBytes: 64 * 1024);
    if (!mounted) return;
    await showDialog<void>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(s.log),
        content: SizedBox(
          width: double.maxFinite,
          child: SingleChildScrollView(
            child: SelectableText(
              text.isEmpty ? s.logEmpty : text,
              style: const TextStyle(fontFamily: 'monospace', fontSize: 11),
            ),
          ),
        ),
        actions: [
          if (text.isNotEmpty)
            TextButton(
              onPressed: () async {
                await Clipboard.setData(ClipboardData(text: text));
                if (context.mounted) Navigator.pop(context);
                if (mounted) _say(s.copied);
              },
              child: Text(s.copy),
            ),
          TextButton(onPressed: () => Navigator.pop(context), child: Text(s.ok)),
        ],
      ),
    );
  }

  /// Every memo as a zip of Markdown files, through the system's own "save as". The zip is
  /// plaintext and exists only in memory until the user names a place for it.
  Future<void> _export() async {
    final s = widget.strings;
    try {
      final zip = await exportMarkdownZip();
      final now = DateTime.now();
      String two(int n) => n.toString().padLeft(2, '0');
      final ok = await host.saveAs(
        name: 'Ymemo-${now.year}-${two(now.month)}-${two(now.day)}.zip',
        mime: 'application/zip',
        bytes: zip,
      );
      if (ok) _say(s.exported);
    } catch (e) {
      _say('${s.exportFailed}: $e');
    }
  }

  void _say(String message) => ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text(message), duration: const Duration(seconds: 2)),
      );

  Future<void> _checkNow() async {
    setState(() {
      _checking = true;
      _updateState = _UpdateState.checking;
    });
    await widget.settings.markUpdateChecked();
    try {
      final release = await updateCheck();
      if (!mounted) return;
      setState(() {
        _update = release;
        _updateState = release == null ? _UpdateState.latest : _UpdateState.found;
      });
    } catch (e) {
      // The core's message, already in the language it was raised in.
      if (mounted) {
        setState(() {
          _updateError = '$e';
          _updateState = _UpdateState.failed;
        });
      }
    } finally {
      if (mounted) setState(() => _checking = false);
    }
  }

  /// The status line, built from the state so it follows the language.
  String? get _updateStatus => switch (_updateState) {
        _UpdateState.idle => null,
        _UpdateState.checking => widget.strings.updateChecking,
        _UpdateState.latest => widget.strings.updateLatest,
        _UpdateState.found => '${widget.strings.updateAvailable} ${_update?.version ?? ''}',
        _UpdateState.failed => _updateError,
      };

  @override
  Widget build(BuildContext context) {
    final s = widget.strings;
    // The desktop's settings pages, as sections: general, notes, lock and security, sync,
    // data, about. Every setting carries its whole explanation, written to be read in full —
    // it used to be cut to a first sentence with an ⓘ for the rest, which read as a column of
    // half-finished sentences.
    return Scaffold(
      appBar: AppBar(title: Text(s.settings)),
      body: ListView(
        padding: EdgeInsets.fromLTRB(0, 8, 0, 8 + bottomInset(context)),
        children: [
          _header(s.sectionGeneral),
          // Language names stay untranslated: written in their own language they are findable
          // even by someone stuck in one they cannot read.
          ListTile(
            title: Text(s.language),
            trailing: DropdownButton<String>(
              value: const ['auto', 'ko', 'en'].contains(_s.lang) ? _s.lang : 'auto',
              onChanged: (v) => v == null ? null : _setLanguage(v),
              items: [
                DropdownMenuItem(value: 'auto', child: Text(s.languageAuto)),
                const DropdownMenuItem(value: 'ko', child: Text('한국어')),
                const DropdownMenuItem(value: 'en', child: Text('English')),
              ],
            ),
          ),

          const Divider(),
          _header(s.sectionNotes),
          ListTile(title: Text(s.defaultColor)),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 8),
            child: ColorSwatches(
              strings: s,
              selected: _s.defaultColor,
              onPick: (key) async {
                await _save(defaultColor: key);
                if (mounted) setState(() {});
              },
            ),
          ),

          const Divider(),
          _header(s.sectionLock),
          SwitchListTile(
            value: _s.lockOnBackground,
            onChanged: (v) => _save(lockOnBackground: v),
            title: Text(s.lockOnBackground),
            subtitle: Text(s.lockOnBackgroundHint),
          ),
          SwitchListTile(
            value: _s.biometricUnlock,
            onChanged: _setBiometric,
            title: Text(s.biometricUnlock),
            subtitle: Text(s.biometricUnlockHint),
          ),
          ListTile(
            title: Text(s.unlockDays),
            subtitle: Text(s.unlockDaysHint),
            trailing: DropdownButton<int>(
              value: _dayChoices.contains(_s.unlockDays) ? _s.unlockDays : 0,
              onChanged: (v) => v == null ? null : _setUnlockDays(v),
              items: [
                // The unit on every choice, 0 included, as the retention list does.
                for (final days in _dayChoices)
                  DropdownMenuItem(value: days, child: Text('$days ${s.daysUnit}')),
              ],
            ),
          ),
          ListTile(
            leading: const Icon(Icons.password),
            title: Text(s.securityTitle),
            subtitle: Text(s.securityHint),
            trailing: const Icon(Icons.chevron_right),
            onTap: () => Navigator.of(context).push(MaterialPageRoute(
              builder: (_) => SecurityScreen(
                strings: widget.strings,
                vaultDir: widget.vaultDir,
              ),
            )),
          ),
          ListTile(
            leading: const Icon(Icons.lock_outline),
            title: Text(s.lockNow),
            subtitle: Text(s.lockNowHint),
            // No pop here. Locking already pops back to the root and swaps in the lock
            // screen; popping again would take the root with it and leave a black screen.
            onTap: widget.onLock,
          ),

          const Divider(),
          _header(s.sectionSync),
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
            child: Text(s.advancedHint, style: Theme.of(context).textTheme.bodySmall),
          ),
          // The three together are what decides how fast a change appears: the sending
          // device's watch delay plus the receiving one's pull interval. Splitting them
          // across the screen would hide that they add up.
          _seconds(s.watchDelay, s.watchDelayHint, _watchChoices, _s.watchDelaySeconds,
              (v) => _save(watchDelaySeconds: v)),
          _seconds(s.mergeSeconds, s.mergeSecondsHint, _mergeChoices, _s.mergeSeconds,
              (v) => _save(mergeSeconds: v)),
          _seconds(s.rescan, s.rescanHint, _rescanChoices, _s.rescanSeconds,
              (v) => _save(rescanSeconds: v)),
          SwitchListTile(
            value: _s.wifiOnlySync,
            onChanged: (v) => _save(wifiOnlySync: v),
            title: Text(s.wifiOnly),
            subtitle: Text(s.wifiOnlyHint),
          ),
          ListTile(
            title: Text(s.keepVersions),
            subtitle: Text(s.keepVersionsHint),
            trailing: DropdownButton<int>(
              value: _keepChoices.contains(_s.keepVersionsDays) ? _s.keepVersionsDays : 30,
              onChanged: (v) => v == null ? null : _save(keepVersionsDays: v),
              items: [
                for (final days in _keepChoices)
                  DropdownMenuItem(value: days, child: Text('$days ${s.daysUnit}')),
              ],
            ),
          ),

          // Export and the problem log are about the vault and the app, not about how fast
          // a change travels; the desktop keeps them on a page of their own, and so does this.
          const Divider(),
          _header(s.dataSection),
          ListTile(
            title: Text(s.exportTitle),
            subtitle: Text(s.exportHint),
            trailing: TextButton(onPressed: _export, child: Text(s.exportButton)),
          ),
          // A phone has no file manager worth sending someone to, so the log is shown here
          // and offered for copying rather than pointed at.
          ListTile(
            title: Text(s.log),
            subtitle: Text(s.logHint),
            trailing: TextButton(onPressed: _showLog, child: Text(s.logView)),
          ),

          const Divider(),
          _header(s.sectionAbout),
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
            child: Row(
              children: [
                Image.asset('assets/logo.png', width: 44, height: 44),
                const SizedBox(width: 14),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text('Ymemo',
                          style: Theme.of(context)
                              .textTheme
                              .titleMedium
                              ?.copyWith(fontWeight: FontWeight.w700)),
                      Text(s.tagline, style: Theme.of(context).textTheme.bodySmall),
                    ],
                  ),
                ),
              ],
            ),
          ),
          SwitchListTile(
            value: _s.updateCheck,
            onChanged: (v) => _save(updateCheck: v),
            title: Text(s.updateCheck),
            subtitle: Text(s.updateCheckHint),
          ),
          // The version, and what the last check said about it; tapping asks again.
          FutureBuilder<String>(
            future: _version,
            builder: (context, snapshot) => ListTile(
              title: Text('${s.version} ${snapshot.data ?? ''}'),
              subtitle: _updateStatus == null ? null : Text(_updateStatus!),
              trailing: _checking
                  ? const SizedBox(
                      width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2))
                  : IconButton(
                      icon: const Icon(Icons.refresh),
                      tooltip: s.updateNow,
                      onPressed: _checkNow,
                    ),
            ),
          ),
          if (_update != null)
            ListTile(
              leading: const Icon(Icons.download),
              title: Text(s.updateOpen),
              // The apk for this phone's ABI, named. A release carries three of them and the
              // release page cannot tell you which is yours.
              subtitle: _update!.file.isEmpty
                  ? null
                  : Text(_update!.file,
                      style: const TextStyle(fontFamily: 'monospace', fontSize: 11)),
              onTap: () => host.openUrl(_update!.url),
            ),
          ListTile(
            title: Text(s.projectPage),
            subtitle: const Text('github.com/PfClaKr/Ymemo'),
            trailing: TextButton(
              onPressed: () => host.openUrl('https://github.com/PfClaKr/Ymemo'),
              child: Text(s.open),
            ),
          ),
        ],
      ),
    );
  }

  Widget _header(String text) => Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
        child: Text(
          text,
          style: Theme.of(context)
              .textTheme
              .titleSmall
              ?.copyWith(color: Theme.of(context).colorScheme.primary),
        ),
      );
}
