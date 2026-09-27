import 'package:flutter_test/flutter_test.dart';
import 'package:ymemo_mobile/memo_title.dart';

void main() {
  test('the snippet is the match in its words', () {
    final hit = searchSnippet('회의록\n결정 사항\n배포는 금요일에 합니다. 담당은 윤', '금요일')!;
    expect(hit.match, '금요일');
    expect(hit.before.startsWith('…'), isTrue);
    expect(hit.after.startsWith('에 합니다'), isTrue);
  });

  test('it starts on a word, not halfway into one', () {
    final hit = searchSnippet('Decisions ship on Friday', 'friday')!;
    expect(hit.before, '…ship on ');
  });

  test('case does not matter, and no match is none', () {
    expect(searchSnippet('Hello World', 'world')!.match, 'World');
    expect(searchSnippet('nothing here', 'zzz'), isNull);
    expect(searchSnippet('anything', '  '), isNull);
  });
}
