import 'package:flutter_test/flutter_test.dart';

import 'package:agentpad/pointer.dart';

void main() {
  test(
    'pointerHzForDisplay doubles refresh rate and floors to a supported tier',
    () {
      expect(pointerHzForDisplay(null), 60);
      expect(pointerHzForDisplay(0), 60);
      expect(pointerHzForDisplay(-60), 60);
      expect(pointerHzForDisplay(double.nan), 60);
      expect(pointerHzForDisplay(double.infinity), 60);
      expect(pointerHzForDisplay(24), 60);
      expect(pointerHzForDisplay(60), 120);
      expect(pointerHzForDisplay(60.000004), 120);
      expect(pointerHzForDisplay(75), 120);
      expect(pointerHzForDisplay(89), 120);
      expect(pointerHzForDisplay(90), 120);
      expect(pointerHzForDisplay(120), 240);
      expect(pointerHzForDisplay(120.00001), 240);
      expect(pointerHzForDisplay(144), 240);
      expect(pointerHzForDisplay(230), 320);
      expect(pointerHzForDisplay(1000), 960);
      for (var i = 0; i < pointerHzGears.length; i++) {
        final hz = pointerHzGears[i];
        expect(pointerHzForDisplay(hz / 2), hz);
        if (i > 0) {
          expect(pointerHzForDisplay(hz / 2 - 0.001), pointerHzGears[i - 1]);
        }
      }
    },
  );

  test('coalesces motion only while the previous send is busy', () {
    final p = PointerCoalescer();
    p.add(4, 6, 0, 0);
    p.add(2, 2, 0, 0);
    expect(p.tick(), {'dx': 6, 'dy': 8, 'buttons': 0, 'wheel': 0});
    expect(p.tick(), isNull);

    p.add(2, 3, 0, 1);
    expect(p.tick(), {'dx': 2, 'dy': 3, 'buttons': 0, 'wheel': 1});

    p.add(0, 0, 0, 0.25);
    p.add(0, 0, 0, 0.25);
    expect(p.tick()!['wheel'], 0.5);
    p.add(0, 0, 0, -0.25);
    expect(p.tick()!['wheel'], -0.25);
  });

  test(
    'cadence waits one period from the first sample and ignores later ones',
    () {
      final c = PointerCadence();
      expect(c.due(Duration.zero), isFalse);
      expect(c.due(const Duration(milliseconds: 8)), isFalse);
      expect(c.due(const Duration(milliseconds: 15)), isFalse);
      expect(c.due(const Duration(milliseconds: 16)), isTrue);
      expect(c.due(const Duration(milliseconds: 20)), isFalse);
      expect(c.due(const Duration(milliseconds: 32)), isTrue);
    },
  );

  test('cadence period can be updated after construction', () {
    final c = PointerCadence();
    for (final hz in pointerHzGears) {
      c.reset();
      c.period = Duration(microseconds: (1000000 / hz).round());
      expect(c.due(Duration.zero), isFalse);
      expect(c.due(c.period - const Duration(microseconds: 1)), isFalse);
      expect(c.due(c.period), isTrue);
      expect(c.due(c.period), isFalse);
    }
  });

  test('late hitch still emits one slot instead of catching up in a burst', () {
    final c = PointerCadence();
    expect(c.due(Duration.zero), isFalse);
    expect(c.due(const Duration(milliseconds: 50)), isTrue);
    expect(c.due(const Duration(milliseconds: 50)), isFalse);
    expect(c.due(const Duration(milliseconds: 64)), isTrue);
  });

  test(
    'markSent restarts the grid without using the next sample as a delay origin',
    () {
      final c = PointerCadence();
      c.markSent(const Duration(milliseconds: 5));
      expect(c.due(const Duration(milliseconds: 10)), isFalse);
      expect(c.due(const Duration(milliseconds: 21)), isTrue);
    },
  );
}
