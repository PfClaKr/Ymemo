/// What an error says, for putting in front of the user.
library;

import 'package:flutter/services.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge.dart';

/// The message an error carries, without the type it came wrapped in.
///
/// Every failure from the core arrives as an [AnyhowException], whose `toString` is
/// `AnyhowException(<message>)` — and `'$e'` is what the screens showed, so a wrong password
/// read "AnyhowException(비밀번호가 틀렸습니다. 다시 입력하세요)" under the field. The core
/// already words its errors for people, in the app's language; this hands over just that.
String errorMessage(Object error) => switch (error) {
      AnyhowException(:final message) => message,
      PlatformException(:final message, :final code) => message ?? code,
      _ => '$error',
    };
