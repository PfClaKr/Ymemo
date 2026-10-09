/// Scanning another device's pairing QR.
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

import '../src/rust/api.dart';
import '../sync.dart';

/// Scans another device's pairing QR and registers it.
///
/// The core validates the format and does the registering (`syncPairWith`), so a change to
/// either leaves Dart alone. This is only ever half the job — the scanned device has to be
/// given this one's code too — which is what the message on the way out says.
class ScanScreen extends StatefulWidget {
  const ScanScreen({super.key, required this.strings, required this.sync});

  final FfiStrings strings;
  final SyncController sync;

  @override
  State<ScanScreen> createState() => _ScanScreenState();
}

class _ScanScreenState extends State<ScanScreen> {
  final _controller = MobileScannerController(
    // Only pairing QRs matter, so other barcode formats are ignored: fewer false hits, less battery.
    formats: const [BarcodeFormat.qrCode],
    detectionSpeed: DetectionSpeed.noDuplicates,
  );
  // Stop after the first hit; the camera keeps streaming the same code.
  bool _handled = false;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  Future<void> _onDetect(BarcodeCapture capture) async {
    if (_handled) return;
    final raw = capture.barcodes
        .map((b) => b.rawValue)
        .firstWhere((v) => v != null && v.isNotEmpty, orElse: () => null);
    if (raw == null) return;
    _handled = true;

    final messenger = ScaffoldMessenger.of(context);
    final navigator = Navigator.of(context);
    try {
      final peer = await widget.sync.pairWith(raw);
      await _controller.stop();
      // Pops with the peer id: this device is now dialling one that has never heard of it,
      // and the screen underneath turns that into "waiting to be allowed in".
      navigator.pop(peer);
    } catch (e) {
      // Show the core's message as-is and allow another scan.
      messenger.showSnackBar(SnackBar(content: Text('$e')));
      _handled = false;
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: Text(widget.strings.scanQr)),
      body: Stack(
        fit: StackFit.expand,
        children: [
          MobileScanner(
            controller: _controller,
            onDetect: _onDetect,
            // A camera that cannot open (denied, or absent) must not leave a blank screen.
            errorBuilder: (context, error) => Center(
              child: Padding(
                padding: const EdgeInsets.all(24),
                child: Text(
                  '${widget.strings.cameraError}\n\n${error.errorCode.name}',
                  textAlign: TextAlign.center,
                ),
              ),
            ),
          ),
          Align(
            alignment: Alignment.bottomCenter,
            child: Container(
              width: double.infinity,
              color: Colors.black54,
              // Clear of the gesture bar, which the app draws under edge to edge.
              padding: EdgeInsets.fromLTRB(
                  24, 16, 24, 16 + MediaQuery.paddingOf(context).bottom),
              child: Text(
                widget.strings.scanHint,
                textAlign: TextAlign.center,
                style: const TextStyle(color: Colors.white),
              ),
            ),
          ),
        ],
      ),
    );
  }
}
