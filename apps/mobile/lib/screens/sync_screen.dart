/// Linking devices: this one's code, pairing, the 6-digit LAN code and the devices the
/// vault is shared with.
library;

import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../src/rust/api.dart';
import '../sync.dart';
import '../ui_util.dart';
import 'scan_screen.dart';

/// App-bar button opening the sync screen, with the daemon's state on its face: a spinner
/// while it starts, a struck-through icon when there is nothing to sync with.
class SyncButton extends StatelessWidget {
  const SyncButton({super.key, required this.strings, required this.sync});

  final FfiStrings strings;
  final SyncController sync;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: sync,
      builder: (context, _) {
        final Widget state;
        if (sync.starting) {
          state = const SizedBox(
            width: 18,
            height: 18,
            child: CircularProgressIndicator(strokeWidth: 2),
          );
        } else if (sync.running) {
          state = const Icon(Icons.devices);
        } else {
          state = const Icon(Icons.cloud_off);
        }
        // A request only reaches this device while the app is open, so it has to be visible
        // from the screen the user is already on rather than only inside the pairing screen.
        final icon = sync.pending.isEmpty
            ? state
            : Badge(label: Text('${sync.pending.length}'), child: state);
        return IconButton(
          icon: icon,
          tooltip: strings.syncDevices,
          onPressed: () => open(context, strings, sync),
        );
      },
    );
  }

  /// Opens the pairing screen. Shared with the first-run "connect to another device" card,
  /// so the two cannot drift into opening different things.
  static Future<void> open(
      BuildContext context, FfiStrings strings, SyncController sync) {
    return Navigator.of(context).push(
      MaterialPageRoute(builder: (_) => SyncScreen(strings: strings, sync: sync)),
    );
  }
}

/// Pairing and the list of paired devices.
///
/// Pairing is **mutual**: adding the other device here only opens this side. Hence the code
/// at the top — the other device has to be given it, by QR from the desktop's window or by
/// typing it in. Until both sides have done their half, syncthing never connects the two.
class SyncScreen extends StatefulWidget {
  const SyncScreen({super.key, required this.strings, required this.sync});

  final FfiStrings strings;
  final SyncController sync;

  @override
  State<SyncScreen> createState() => _SyncScreenState();
}

class _SyncScreenState extends State<SyncScreen> {
  /// How often the shown code is re-read and incoming pairings are collected. The code
  /// rotates once a minute; this is only so the screen never shows a stale one.
  static const _lanPollInterval = Duration(seconds: 1);

  /// How often the device we asked is checked for having answered.
  static const _waitPollInterval = Duration(seconds: 2);

  /// Consecutive polls the peer must look connected before this is called a link.
  ///
  /// One is not enough: while the request is unanswered the peer's handshake completes and is
  /// *then* refused, so `connected` flickers true for a fraction of a second on every retry.
  /// Two polls two seconds apart never straddle that.
  static const _linkedPolls = 2;

  List<FfiSharedDevice> _devices = const [];

  final _lanInput = TextEditingController();
  /// The other device's pairing code, typed or pasted rather than scanned.
  final _peerInput = TextEditingController();
  bool _adding = false;
  String? _lanCode;
  String? _lanMessage;
  bool _joining = false;
  Timer? _lanPoll;

  /// The device this one scanned and is waiting to be allowed in by, with the eight
  /// characters its screen is showing. Null when nothing is outstanding.
  String? _waitingPeer;
  String? _waitingCode;
  int _connectedPolls = 0;
  Timer? _waitPoll;

  @override
  void initState() {
    super.initState();
    _reloadDevices();
    _startLan();
  }

  @override
  void dispose() {
    _lanPoll?.cancel();
    _waitPoll?.cancel();
    _lanInput.dispose();
    _peerInput.dispose();
    // Leaves pairing mode: closes the socket and drops the wifi multicast lock. Anything
    // still in flight is finished by the Rust side on its own thread.
    widget.sync.lanStop();
    super.dispose();
  }

  /// Enters pairing mode for as long as this screen is open.
  Future<void> _startLan() async {
    try {
      final code = await widget.sync.lanStart();
      if (!mounted) return;
      setState(() => _lanCode = code);
      if (code != null) {
        _lanPoll = Timer.periodic(_lanPollInterval, (_) => _pollLan());
      }
    } catch (e) {
      if (mounted) setState(() => _lanMessage = '$e');
    }
  }

  /// Refreshes the displayed code and picks up devices that used it. The Rust side has
  /// already registered them; this only has to say so and redraw the list.
  Future<void> _pollLan() async {
    try {
      final code = await widget.sync.lanCode();
      if (code == null) {
        // Backgrounding the app leaves pairing mode; coming back re-enters it. Null again
        // just means the daemon is not up yet, and the next tick tries once more.
        final restarted = await widget.sync.lanStart();
        if (mounted) setState(() => _lanCode = restarted);
        return;
      }
      final paired = await widget.sync.lanPollPaired();
      if (!mounted) return;
      setState(() {
        _lanCode = code;
        if (paired.isNotEmpty) _lanMessage = widget.strings.lanDone;
      });
      if (paired.isNotEmpty) await _reloadDevices();
    } catch (e) {
      debugPrint('lan poll failed: $e');
    }
  }

  /// Joiner side: broadcast for the device showing the typed code.
  Future<void> _joinLan() async {
    final code = _lanInput.text.trim();
    // Six digits or nothing: anything else only failed after the whole network wait.
    if (code.length != 6 || _joining) return;
    setState(() {
      _joining = true;
      _lanMessage = widget.strings.lanSearching;
    });
    try {
      final peer = await widget.sync.lanJoin(code);
      if (!mounted) return;
      setState(() {
        _lanMessage = peer == null ? widget.strings.lanNotFound : widget.strings.lanDone;
        if (peer != null) _lanInput.clear();
      });
      if (peer != null) await _reloadDevices();
    } catch (e) {
      // A malformed code is rejected by the core, in the user's language.
      if (mounted) setState(() => _lanMessage = '$e');
    } finally {
      if (mounted) setState(() => _joining = false);
    }
  }

  Future<void> _reloadDevices() async {
    try {
      final devices = await widget.sync.devices();
      if (mounted) setState(() => _devices = devices);
    } catch (e) {
      debugPrint('could not list devices: $e');
    }
  }

  Future<void> _copyCode() async {
    final code = widget.sync.pairingCode;
    if (code == null) return;
    await Clipboard.setData(ClipboardData(text: code));
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text(widget.strings.copied)),
    );
  }

  Future<void> _scan() async {
    final peer = await Navigator.of(context).push<String>(
      MaterialPageRoute(
        builder: (_) => ScanScreen(strings: widget.strings, sync: widget.sync),
      ),
    );
    if (peer != null) await _startWaiting(peer);
    await _reloadDevices();
  }

  /// Registers a peer from a code that was typed or pasted.
  ///
  /// The same half of pairing that scanning does — the core validates the code and does the
  /// registering — for the cases a camera cannot cover: a device with no working camera, a
  /// desktop across the room whose QR is not in front of you, or a code sent in a message.
  Future<void> _addTypedPeer() async {
    final raw = _peerInput.text.trim();
    if (raw.isEmpty || _adding) return;
    setState(() => _adding = true);
    final messenger = ScaffoldMessenger.of(context);
    try {
      final peer = await widget.sync.pairWith(raw);
      _peerInput.clear();
      await _startWaiting(peer);
      await _reloadDevices();
    } catch (e) {
      // The core's message says what is wrong with the code; show it as it is.
      messenger.showSnackBar(SnackBar(content: Text('$e')));
    } finally {
      if (mounted) setState(() => _adding = false);
    }
  }

  /// Enters the waiting state for a peer that has just been registered.
  ///
  /// Scanning only did this device's half: it is dialling a device that has never heard of
  /// it, and nothing syncs until that device allows the request.
  Future<void> _startWaiting(String peer) async {
    String code = '';
    try {
      code = await widget.sync.verificationCode(peer);
    } catch (e) {
      // Without our own device id there is nothing to derive it from. The request still
      // works; only the code the user would compare is missing.
      debugPrint('could not derive the verification code: $e');
    }
    if (!mounted) return;
    setState(() {
      _waitingPeer = peer;
      _waitingCode = code;
      _connectedPolls = 0;
      _lanMessage = null;
    });
    _waitPoll?.cancel();
    _waitPoll = Timer.periodic(_waitPollInterval, (_) => _checkWaiting());
  }

  void _stopWaiting({String? message}) {
    _waitPoll?.cancel();
    _waitPoll = null;
    if (!mounted) return;
    setState(() {
      _waitingPeer = null;
      _waitingCode = null;
      _connectedPolls = 0;
      if (message != null) _lanMessage = message;
    });
  }

  /// Has the device we asked let us in yet?
  Future<void> _checkWaiting() async {
    final peer = _waitingPeer;
    if (peer == null) return;
    final devices = await widget.sync.devices();
    if (!mounted) return;
    final up = devices.any((d) => d.id == peer && d.connected);
    _connectedPolls = up ? _connectedPolls + 1 : 0;
    setState(() => _devices = devices);
    if (_connectedPolls >= _linkedPolls) {
      _stopWaiting(message: widget.strings.pairConnected);
    }
  }

  Future<void> _approve(FfiPendingDevice device) async {
    try {
      await widget.sync.approveDevice(device.id);
    } catch (e) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('$e')));
    }
    await _reloadDevices();
  }

  Future<void> _reject(FfiPendingDevice device) async {
    try {
      await widget.sync.rejectDevice(device.id);
    } catch (e) {
      debugPrint('could not reject the request: $e');
    }
  }

  Future<void> _unpair(FfiSharedDevice device) async {
    // The removal is written into the synced vault and only a fresh pairing undoes it.
    final sure = await confirmAction(
      context,
      message: widget.strings.unpairWarning,
      confirm: widget.strings.unpair,
      cancel: widget.strings.cancel,
    );
    if (!sure || !mounted) return;
    try {
      await widget.sync.unpair(device.id);
    } catch (e) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('$e')));
    }
    await _reloadDevices();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text(widget.strings.syncDevices),
        actions: [
          IconButton(
            icon: const Icon(Icons.refresh),
            tooltip: widget.strings.syncNow,
            onPressed: _reloadDevices,
          ),
        ],
      ),
      body: ListenableBuilder(
        listenable: widget.sync,
        builder: (context, _) => ListView(
          padding: EdgeInsets.fromLTRB(16, 16, 16, 16 + bottomInset(context)),
          children: [
            // Requests first: someone is standing at another device waiting for this tap.
            for (final request in widget.sync.pending) ...[
              _requestCard(context, request),
              const SizedBox(height: 12),
            ],
            _status(context),
            if (_waitingPeer != null) ...[
              const Divider(height: 32),
              _waitingSection(context),
            ],
            if (_lanCode != null || _lanMessage != null) ...[
              const Divider(height: 32),
              _lanSection(context),
            ],
            const Divider(height: 32),
            // Not `syncDevices` — that is this screen's own title, and the same words twice
            // on one screen read as a heading that lost its section. This one is the list of
            // devices already paired.
            Text(widget.strings.connectedDevices,
                style: Theme.of(context).textTheme.titleMedium),
            const SizedBox(height: 8),
            if (_devices.isEmpty)
              Text(widget.strings.noDevices,
                  style: Theme.of(context).textTheme.bodySmall)
            else
              for (final device in _devices)
                ListTile(
                  contentPadding: EdgeInsets.zero,
                  leading: Icon(
                    device.connected ? Icons.link : Icons.link_off,
                    color: device.connected ? Colors.green : null,
                  ),
                  title: Text(device.name.isEmpty ? device.id : device.name),
                  subtitle: Text(device.connected
                      ? widget.strings.connected
                      : widget.strings.disconnected),
                  trailing: IconButton(
                    icon: const Icon(Icons.delete_outline),
                    tooltip: widget.strings.unpair,
                    onPressed: () => _unpair(device),
                  ),
                ),
          ],
        ),
      ),
    );
  }

  /// Pairing over the local network: six digits instead of a 63-character device id.
  ///
  /// Both directions are offered because either device can be the one doing the typing, and
  /// whichever way round it goes, one exchange registers **both** sides.
  Widget _lanSection(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(widget.strings.lanPairing, style: Theme.of(context).textTheme.titleMedium),
        if (_lanCode != null) ...[
          const SizedBox(height: 8),
          Text(widget.strings.lanMyCode, style: Theme.of(context).textTheme.bodySmall),
          const SizedBox(height: 4),
          Text(
            // Spaced out, because this gets read aloud across a room.
            _lanCode!.split('').join(' '),
            style: const TextStyle(fontSize: 30, letterSpacing: 2, fontFeatures: [ui.FontFeature.tabularFigures()]),
          ),
        ],
        const SizedBox(height: 12),
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: TextField(
                controller: _lanInput,
                keyboardType: TextInputType.number,
                maxLength: 6,
                decoration: InputDecoration(
                  labelText: widget.strings.lanEnterCode,
                  counterText: '',
                ),
                onSubmitted: (_) => _joinLan(),
              ),
            ),
            const SizedBox(width: 12),
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: ListenableBuilder(
                listenable: _lanInput,
                builder: (context, _) => FilledButton(
                  onPressed: _joining || _lanInput.text.trim().length != 6 ? null : _joinLan,
                  child: Text(widget.strings.lanConnect),
                ),
              ),
            ),
          ],
        ),
        if (_lanMessage != null)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(_lanMessage!, style: Theme.of(context).textTheme.bodySmall),
          ),
      ],
    );
  }

  /// The top block: what the daemon is doing, and this device's code once it is up.
  /// One incoming request, with the comparison the user is being asked to make.
  ///
  /// A card rather than a dialog: a request can arrive at any moment, and a dialog thrown
  /// over whatever the user was doing is how people tap "allow" without reading it.
  Widget _requestCard(BuildContext context, FfiPendingDevice request) {
    final s = widget.strings;
    final scheme = Theme.of(context).colorScheme;
    return Card(
      color: scheme.secondaryContainer,
      margin: EdgeInsets.zero,
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                const Icon(Icons.device_unknown, size: 20),
                const SizedBox(width: 8),
                Text(s.pairRequest, style: Theme.of(context).textTheme.titleMedium),
              ],
            ),
            const SizedBox(height: 12),
            // The name is the asking device's own choice, so it never stands in for the id.
            if (request.name.isNotEmpty)
              Text(request.name, style: Theme.of(context).textTheme.bodyLarge),
            Text(s.deviceId, style: Theme.of(context).textTheme.labelSmall),
            SelectableText(
              request.id,
              style: const TextStyle(fontFamily: 'monospace', fontSize: 11),
            ),
            const SizedBox(height: 12),
            Text(s.pairVerify, style: Theme.of(context).textTheme.bodySmall),
            const SizedBox(height: 4),
            Center(
              child: Text(
                request.verificationCode,
                style: const TextStyle(
                  fontFamily: 'monospace',
                  fontSize: 28,
                  fontWeight: FontWeight.bold,
                  letterSpacing: 3,
                ),
              ),
            ),
            const SizedBox(height: 8),
            Text(s.pairRequestHint, style: Theme.of(context).textTheme.bodySmall),
            const SizedBox(height: 8),
            Row(
              mainAxisAlignment: MainAxisAlignment.end,
              children: [
                TextButton(
                  onPressed: () => _reject(request),
                  child: Text(s.reject),
                ),
                const SizedBox(width: 8),
                FilledButton(
                  onPressed: () => _approve(request),
                  child: Text(s.allow),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  /// The other side of the same moment: this device asked, and is waiting to be let in.
  Widget _waitingSection(BuildContext context) {
    final s = widget.strings;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            const SizedBox(
              width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2)),
            const SizedBox(width: 12),
            Expanded(
              child: Text(s.pairWaiting, style: Theme.of(context).textTheme.titleMedium),
            ),
          ],
        ),
        const SizedBox(height: 8),
        Text(s.pairWaitingHint, style: Theme.of(context).textTheme.bodySmall),
        if ((_waitingCode ?? '').isNotEmpty) ...[
          const SizedBox(height: 12),
          Text(s.pairVerification, style: Theme.of(context).textTheme.labelSmall),
          Center(
            child: Text(
              _waitingCode!,
              style: const TextStyle(
                fontFamily: 'monospace',
                fontSize: 28,
                fontWeight: FontWeight.bold,
                letterSpacing: 3,
              ),
            ),
          ),
        ],
        const SizedBox(height: 8),
        Align(
          alignment: Alignment.centerRight,
          // The link itself is already registered and keeps retrying; this only takes the
          // panel down for someone who would rather not watch it.
          child: TextButton(
            onPressed: () => _stopWaiting(),
            child: Text(s.pairCancelWait),
          ),
        ),
      ],
    );
  }

  Widget _status(BuildContext context) {
    final sync = widget.sync;
    if (!sync.available) {
      return Text(widget.strings.syncUnavailable);
    }
    if (sync.starting) {
      return Row(
        children: [
          const SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2)),
          const SizedBox(width: 12),
          Text(widget.strings.syncStarting),
        ],
      );
    }
    final code = sync.pairingCode;
    if (code == null) {
      // Started and failed: show the core's message rather than a bare "unavailable".
      return Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(sync.error ?? widget.strings.syncUnavailable,
              style: const TextStyle(color: Colors.red)),
          const SizedBox(height: 8),
          OutlinedButton(onPressed: sync.start, child: Text(widget.strings.syncNow)),
        ],
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        // A folder held back by "Wi-Fi only" looks exactly like sync being broken, so the
        // reason is said here rather than left to be guessed at.
        if (sync.pausedForMetered)
          Padding(
            padding: const EdgeInsets.only(bottom: 12),
            child: Row(children: [
              const Icon(Icons.pause_circle_outline, size: 18),
              const SizedBox(width: 8),
              Expanded(child: Text(widget.strings.pausedMetered)),
            ]),
          ),
        Text(widget.strings.myCode, style: Theme.of(context).textTheme.titleMedium),
        const SizedBox(height: 8),
        SelectableText(code, style: const TextStyle(fontFamily: 'monospace')),
        const SizedBox(height: 8),
        Wrap(
          spacing: 8,
          children: [
            OutlinedButton.icon(
              onPressed: _copyCode,
              icon: const Icon(Icons.copy, size: 18),
              label: Text(widget.strings.copy),
            ),
            FilledButton.icon(
              onPressed: _scan,
              icon: const Icon(Icons.qr_code_scanner, size: 18),
              label: Text(widget.strings.scanQr),
            ),
          ],
        ),
        const SizedBox(height: 16),
        Text(widget.strings.peerCodeHint, style: Theme.of(context).textTheme.bodySmall),
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: TextField(
                controller: _peerInput,
                autocorrect: false,
                enableSuggestions: false,
                decoration: InputDecoration(labelText: widget.strings.peerCode),
                onSubmitted: (_) => _addTypedPeer(),
              ),
            ),
            const SizedBox(width: 12),
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: FilledButton(
                onPressed: _adding ? null : _addTypedPeer,
                child: Text(widget.strings.addDevice),
              ),
            ),
          ],
        ),
      ],
    );
  }
}
