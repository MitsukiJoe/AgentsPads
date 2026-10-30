import 'dart:async';

import 'package:agentpad/ui_pointer_sampling.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  final binding = TestWidgetsFlutterBinding.ensureInitialized();
  late bool originalEnabled;
  late Duration originalOffset;

  setUp(() {
    originalEnabled = binding.resamplingEnabled;
    originalOffset = binding.samplingOffset;
    binding.resamplingEnabled = false;
    binding.samplingOffset = const Duration(milliseconds: -38);
  });
  tearDown(() {
    binding.resamplingEnabled = originalEnabled;
    binding.samplingOffset = originalOffset;
  });

  Widget scope(double hz, {Key? key}) => UiPointerSampling(
    key: key,
    refreshRate: () async => hz,
    child: const SizedBox(),
  );

  testWidgets('120 Hz panel aligns touches and restores raw input on exit', (
    tester,
  ) async {
    await tester.pumpWidget(scope(120));
    expect(GestureBinding.instance.resamplingEnabled, isTrue);
      expect(binding.samplingOffset, const Duration(milliseconds: -6));
    await tester.pumpWidget(const SizedBox());
    expect(binding.resamplingEnabled, isFalse);
    expect(binding.samplingOffset, const Duration(milliseconds: -38));
  });

  testWidgets('60 Hz panel preserves the existing sampling configuration', (
    tester,
  ) async {
    binding.resamplingEnabled = true;
    binding.samplingOffset = const Duration(milliseconds: -3);
    await tester.pumpWidget(scope(60));
    expect(binding.resamplingEnabled, isTrue);
    expect(binding.samplingOffset, const Duration(milliseconds: -3));
    await tester.pumpWidget(const SizedBox());
    expect(binding.resamplingEnabled, isTrue);
    expect(binding.samplingOffset, const Duration(milliseconds: -3));
  });

  testWidgets('overlapping panels restore only after the last panel closes', (
    tester,
  ) async {
    final first = scope(120, key: const ValueKey('first'));
    final second = scope(240, key: const ValueKey('second'));
    await tester.pumpWidget(Column(children: [first, second]));
    expect(binding.samplingOffset, const Duration(microseconds: -4167));
    await tester.pumpWidget(Column(children: [second]));
    expect(binding.resamplingEnabled, isTrue);
    expect(binding.samplingOffset, const Duration(microseconds: -4167));
    await tester.pumpWidget(const SizedBox());
    expect(binding.resamplingEnabled, isFalse);
    expect(binding.samplingOffset, const Duration(milliseconds: -38));
  });

  testWidgets(
    'late refresh-rate response cannot change input after dismissal',
    (tester) async {
      final pending = Completer<double?>();
      await tester.pumpWidget(
        UiPointerSampling(
          refreshRate: () => pending.future,
          child: const SizedBox(),
        ),
      );
      await tester.pumpWidget(const SizedBox());
      pending.complete(120);
      await tester.pump();
      expect(binding.resamplingEnabled, isFalse);
      expect(binding.samplingOffset, const Duration(milliseconds: -38));
      expect(tester.takeException(), isNull);
    },
  );

  testWidgets('unavailable native refresh rate falls back to the display', (
    tester,
  ) async {
    tester.view.display.refreshRate = 120;
    addTearDown(tester.view.display.resetRefreshRate);
    await tester.pumpWidget(
      UiPointerSampling(
        refreshRate: () async => throw StateError('unavailable'),
        child: const SizedBox(),
      ),
    );
    expect(binding.resamplingEnabled, isTrue);
      expect(binding.samplingOffset, const Duration(milliseconds: -6));
    await tester.pumpWidget(const SizedBox());
    expect(binding.resamplingEnabled, isFalse);
  });
}
