// What a failure reads like on screen.
//
// Run with `flutter test` from `apps/mobile`.

import 'package:flutter/services.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:ymemo_mobile/errors.dart';

void main() {
  test('a core error is shown as its message, not as the exception it came in', () {
    expect(errorMessage(AnyhowException('비밀번호가 틀렸습니다. 다시 입력하세요')),
        '비밀번호가 틀렸습니다. 다시 입력하세요');
  });

  test('a platform error says its message, or its code when it has none', () {
    expect(errorMessage(PlatformException(code: 'denied', message: 'No camera')), 'No camera');
    expect(errorMessage(PlatformException(code: 'denied')), 'denied');
  });

  test('anything else is shown as it prints', () {
    expect(errorMessage(StateError('gone')), 'Bad state: gone');
  });
}
