import 'package:agentpad/adaptive_height_column.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  testWidgets('resizes remaining space without speculative child layout', (
    tester,
  ) async {
    var builds = 0;
    final fixed = LayoutBuilder(
      builder: (_, box) {
        builds++;
        return const SizedBox(height: 80);
      },
    );
    Future<void> resize(double height) => tester.pumpWidget(
      Directionality(
        textDirection: TextDirection.ltr,
        child: Center(
          child: SizedBox(
            width: 300,
            height: height,
            child: SingleChildScrollView(
              child: ConstrainedBox(
                constraints: BoxConstraints(minHeight: height),
                child: AdaptiveHeightColumn(
                  fillIndex: 1,
                  minimumFillHeight: 104,
                  children: [
                    fixed,
                    const SizedBox(key: ValueKey('fill')),
                    const SizedBox(height: 40),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
    await resize(500);
    expect(tester.takeException(), isNull);
    expect(tester.getSize(find.byKey(const ValueKey('fill'))).height, 380);
    await resize(400);
    expect(tester.getSize(find.byKey(const ValueKey('fill'))).height, 280);
    expect(builds, 1);
    await resize(180);
    expect(tester.getSize(find.byKey(const ValueKey('fill'))).height, 104);
    expect(
      tester
          .state<ScrollableState>(find.byType(Scrollable))
          .position
          .maxScrollExtent,
      44,
    );
    expect(tester.takeException(), isNull);
  });
}
