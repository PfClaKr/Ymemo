// How a memo is named when it has no title of its own.
//
// Run with `flutter test` from `apps/mobile`.

import 'package:flutter_test/flutter_test.dart';
import 'package:ymemo_mobile/memo_title.dart';
import 'package:ymemo_mobile/src/rust/api.dart';

/// A memo with no title of its own, which is when any of this applies.
FfiMemo _memo(String body) => FfiMemo(
      id: 'id',
      title: '',
      body: body,
      color: 'yellow',
      opacity: 100,
      groupId: '',
      createdAt: 0,
      updatedAt: 0,
      hasPhoto: false,
    );

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

  // The same cases as `a_headings_hashes_are_not_part_of_the_name` in the desktop's
  // sticky.rs; the two derive the same name or a memo reads as two on two devices.
  test("a heading's hashes are not part of the name", () {
    expect(firstLine('```\n# 회의록\n본문\n```'), '회의록');
    expect(firstLine('```\n### Deep\n```'), 'Deep');
    // Outside a markdown region a hash is a character, which is how it is drawn.
    expect(firstLine('# tag\nbody'), '# tag');
    // Inside a fence that named a language it is code, and code is shown as written.
    expect(firstLine('```py\n# comment\n```'), '# comment');
    expect(firstLine('```c\n```\n# still plain'), '# still plain');
    // Not a heading: no space, and too many hashes.
    expect(firstLine('```\n#tag\n```'), '#tag');
    expect(firstLine('```\n####### deep\n```'), '####### deep');
    // A heading with no words names nothing, so the line below is asked instead.
    expect(firstLine('```\n# \n본문\n```'), '본문');
    expect(firstLine('```\n# \n```'), '');
    expect(firstLine('```\n#\n```'), '#');
  });

  test('the preview drops the whole line the name came from', () {
    // Including the hashes, which the name itself does not carry.
    expect(rowPreview(_memo('```\n# 회의록\n본문\n```')), '```\n\n본문\n```');
  });

  test('a long line is capped where the desktop caps it', () {
    expect(firstLine('a' * 60).length, 40);
  });
}
