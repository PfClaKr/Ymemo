// How a memo is named when it has no title of its own.
//
// Run with `flutter test` from `apps/mobile`.

import 'package:flutter_test/flutter_test.dart';
import 'package:ymemo_mobile/memo_title.dart';

void main() {
  test('the name is the first line of the writing', () {
    expect(firstLine('shopping\nmilk'), 'shopping');
    expect(firstLine('\n\n  spaced  \n'), 'spaced');
    expect(firstLine(''), '');
  });

  test('a fence is not a name', () {
    // A memo that opens with a code block is about what is inside it.
    expect(firstLine('```rust\nfn main() {}\n```'), 'fn main() {}');
    expect(firstLine('```\n**bold**\n```'), '**bold**');
  });

  test('a long line is capped where the desktop caps it', () {
    expect(firstLine('a' * 60).length, 40);
  });
}
