// The master password and the recovery code.
//
// Both operations rewrite `vault.json`'s key wrapper and nothing else — no memo is
// re-encrypted, no log or blob is rewritten, no other device is touched — so they finish in
// the time of a couple of Argon2id runs and there is nothing here to show progress for. The
// core is where that is explained (`ymemo_core::vault`).
//
// The one thing this screen is careful about is the code itself: `vaultIssueRecoveryCode`
// returns it once and stores only its wrapper, so a code that scrolls off the screen
// unwritten is gone for good. That is why issuing pushes a page the user has to acknowledge
// rather than dropping a snackbar.

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'src/rust/api.dart';
import 'ui_util.dart';

/// Change the password, and issue or reissue the recovery code.
class SecurityScreen extends StatefulWidget {
  const SecurityScreen({super.key, required this.strings, required this.vaultDir});

  final FfiStrings strings;

  /// Read directly to answer "does this vault have a recovery code", which is a fact about
  /// `vault.json` rather than about the open vault.
  final String vaultDir;

  @override
  State<SecurityScreen> createState() => _SecurityScreenState();
}

class _SecurityScreenState extends State<SecurityScreen> {
  final _current = TextEditingController();
  final _next = TextEditingController();
  final _confirm = TextEditingController();

  bool _hasRecovery = false;
  bool _busy = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    _refreshRecovery();
    // The button follows the fields, as on the desktop: pressing it over empty ones only
    // produced an error about a password nobody had typed yet.
    for (final c in [_current, _next, _confirm]) {
      c.addListener(_fieldsChanged);
    }
  }

  void _fieldsChanged() => setState(() {});

  bool get _canChange =>
      _current.text.isNotEmpty && _next.text.isNotEmpty && _confirm.text.isNotEmpty;

  @override
  void dispose() {
    _current.dispose();
    _next.dispose();
    _confirm.dispose();
    super.dispose();
  }

  Future<void> _refreshRecovery() async {
    final has = await vaultHasRecoveryCode(vaultDir: widget.vaultDir);
    if (mounted) setState(() => _hasRecovery = has);
  }

  Future<void> _changePassword() async {
    final s = widget.strings;
    if (_next.text.characters.length < await passwordMinChars()) {
      setState(() => _error = s.passwordTooShort);
      return;
    }
    if (_next.text != _confirm.text) {
      setState(() => _error = s.passwordMismatch);
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await vaultChangePassword(current: _current.text, newPassword: _next.text);
      _current.clear();
      _next.clear();
      _confirm.clear();
      if (!mounted) return;
      // The stored session key is the *data* key, which a password change does not move, so
      // staying unlocked here is correct rather than an oversight.
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(s.passwordChanged)));
    } catch (e) {
      // Core errors already arrive in the current language.
      if (mounted) setState(() => _error = '$e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _issueRecovery() async {
    // A new code kills the one already written down, so with one on record it asks first.
    if (_hasRecovery) {
      final s = widget.strings;
      final sure = await confirmAction(
        context,
        message: s.reissueWarning,
        confirm: s.reissueRecovery,
        cancel: s.cancel,
      );
      if (!sure || !mounted) return;
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final code = await vaultIssueRecoveryCode();
      if (!mounted) return;
      await showRecoveryCode(context, widget.strings, code);
      await _refreshRecovery();
    } catch (e) {
      if (mounted) setState(() => _error = '$e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final s = widget.strings;
    return Scaffold(
      appBar: AppBar(title: Text(s.securitySection)),
      body: ListView(
        padding: EdgeInsets.fromLTRB(16, 8, 16, 16 + MediaQuery.paddingOf(context).bottom),
        children: [
          _header(s.changePassword),
          Text(s.passwordHint, style: Theme.of(context).textTheme.bodySmall),
          const SizedBox(height: 12),
          TextField(
            controller: _current,
            obscureText: true,
            decoration: InputDecoration(labelText: s.currentPassword),
            textInputAction: TextInputAction.next,
          ),
          const SizedBox(height: 8),
          TextField(
            controller: _next,
            obscureText: true,
            decoration: InputDecoration(labelText: s.newPassword),
            textInputAction: TextInputAction.next,
          ),
          const SizedBox(height: 8),
          TextField(
            controller: _confirm,
            obscureText: true,
            decoration: InputDecoration(labelText: s.confirmPassword),
            onSubmitted: (_) => _busy || !_canChange ? null : _changePassword(),
          ),
          if (_error != null)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(_error!,
                  style: TextStyle(color: Theme.of(context).colorScheme.error)),
            ),
          const SizedBox(height: 12),
          FilledButton(
            onPressed: _busy || !_canChange ? null : _changePassword,
            child: Text(s.changePassword),
          ),

          const Divider(height: 40),
          _header(s.recoveryCode),
          Text(
            _hasRecovery ? s.recoveryPresent : s.recoveryAbsent,
            style: Theme.of(context).textTheme.bodyMedium,
          ),
          const SizedBox(height: 4),
          Text(s.recoveryHint, style: Theme.of(context).textTheme.bodySmall),
          const SizedBox(height: 12),
          OutlinedButton.icon(
            onPressed: _busy ? null : _issueRecovery,
            icon: const Icon(Icons.vpn_key_outlined),
            label: Text(_hasRecovery ? s.reissueRecovery : s.issueRecovery),
          ),
        ],
      ),
    );
  }

  Widget _header(String text) => Padding(
        padding: const EdgeInsets.only(top: 8, bottom: 8),
        child: Text(
          text,
          style: Theme.of(context)
              .textTheme
              .titleMedium
              ?.copyWith(color: Theme.of(context).colorScheme.primary),
        ),
      );
}

/// A recovery code four groups to a line; `recovery_lines` on the desktop.
String recoveryLines(String code) {
  final groups = code.split('-');
  return [
    for (var i = 0; i < groups.length; i += 4)
      groups.sublist(i, i + 4 > groups.length ? groups.length : i + 4).join('-'),
  ].join('\n');
}

/// Shows a freshly issued recovery code and does not leave until it is acknowledged.
///
/// A full page rather than a snackbar or a dismissible dialog, because this is the **only**
/// time the code exists in readable form: `barrierDismissible: false` and no back button, so
/// it cannot be swiped away by accident with nothing written down.
Future<void> showRecoveryCode(BuildContext context, FfiStrings strings, String code) {
  return Navigator.of(context).push<void>(MaterialPageRoute(
    fullscreenDialog: true,
    builder: (context) => PopScope(
      canPop: false,
      child: Scaffold(
        appBar: AppBar(title: Text(strings.recoveryCode), automaticallyImplyLeading: false),
        body: Padding(
          padding:
              EdgeInsets.fromLTRB(24, 24, 24, 24 + MediaQuery.paddingOf(context).bottom),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(strings.recoveryWarning,
                  style: Theme.of(context).textTheme.bodyMedium),
              const SizedBox(height: 20),
              SelectableText(
                // Four groups to a line, as the desktop shows it, so the code breaks between
                // groups rather than wherever the screen ran out. Whitespace is ignored when
                // a code is read back, so a copy of this works as well as the bare code.
                recoveryLines(code),
                textAlign: TextAlign.center,
                style: const TextStyle(
                  // Monospace and wide-spaced: the alphabet already drops the characters
                  // people confuse, and this is what keeps the rest apart when copying by
                  // hand off a phone screen.
                  fontFamily: 'monospace',
                  fontSize: 20,
                  letterSpacing: 2,
                ),
              ),
              const SizedBox(height: 20),
              OutlinedButton.icon(
                onPressed: () async {
                  await Clipboard.setData(ClipboardData(text: code));
                  if (context.mounted) {
                    ScaffoldMessenger.of(context).showSnackBar(
                      SnackBar(
                          content: Text(strings.copied),
                          duration: const Duration(seconds: 1)),
                    );
                  }
                },
                icon: const Icon(Icons.copy),
                label: Text(strings.copy),
              ),
              const Spacer(),
              FilledButton(
                onPressed: () => Navigator.of(context).pop(),
                child: Text(strings.recoveryAck),
              ),
            ],
          ),
        ),
      ),
    ),
  ));
}
