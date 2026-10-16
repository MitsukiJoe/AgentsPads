import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'package:agentpad/app.dart';
import 'package:agentpad/store.dart';

class FailingSaveStore extends PadStore {
  bool failSave = false;
  @override
  Future<void> save() async {
    if (failSave) throw StateError('save failed');
    await super.save();
  }
}

void main() {
  test('restart hint only when a concrete icon is still pending', () {
    expect(appIconRestartHintVisible(null, 'white'), isFalse);
    expect(appIconRestartHintVisible('white', 'white'), isTrue);
    expect(appIconRestartHintVisible('black', 'black'), isTrue);
    expect(appIconRestartHintVisible('black', 'white'), isTrue);
    expect(appIconRestartHintVisible('white', 'black'), isTrue);
    expect(appIconRestartHintVisible('system', 'white'), isFalse);
  });

  test('follow system uses platform brightness, not the app theme', () {
    expect(resolveAppIconTarget('system', Brightness.dark), 'black');
    expect(resolveAppIconTarget('system', Brightness.light), 'white');
    expect(resolveAppIconTarget('white', Brightness.dark), 'white');
    expect(resolveAppIconTarget('black', Brightness.light), 'black');
  });

  testWidgets('only root Back requests native finish', (tester) async {
    SharedPreferences.setMockInitialValues({});
    final calls = <String>[];
    final store = FailingSaveStore();
    const channel = MethodChannel('agentpad/ws');
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, (
      call,
    ) async {
      calls.add(call.method);
      if (call.method == 'getAppIconState') {
        return {'current': 'white', 'pending': null};
      }
      return null;
    });
    addTearDown(
      () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        channel,
        null,
      ),
    );
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('设置'));
    await tester.pumpAndSettle();
    await tester.binding.handlePopRoute();
    await tester.pumpAndSettle();
    expect(calls.where((v) => v == 'finishWithAppIcon'), isEmpty);
    store.failSave = true;
    await tester.binding.handlePopRoute();
    await tester.pumpAndSettle();
    expect(calls.where((v) => v == 'finishWithAppIcon'), isEmpty);
    expect(find.textContaining('save failed'), findsOneWidget);
    store.failSave = false;
    await tester.binding.handlePopRoute();
    await tester.pumpAndSettle();
    expect(calls.where((v) => v == 'finishWithAppIcon'), hasLength(1));
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('icon hint follows pending state and restart is explicit', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    tester.platformDispatcher.platformBrightnessTestValue = Brightness.dark;
    addTearDown(tester.platformDispatcher.clearPlatformBrightnessTestValue);

    var current = 'white';
    String? pending;
    var restarted = false;
    var restartCalls = 0;
    final channel = const MethodChannel('agentpad/ws');
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, (
      call,
    ) async {
      if (call.method == 'setAppIcon') {
        final icon = (call.arguments as Map)['icon'] as String;
        pending = icon == current ? null : icon;
        return pending != null;
      }
      if (call.method == 'getAppIconState') {
        return {'current': current, 'pending': pending};
      }
      if (call.method == 'restartWithAppIcon') {
        restartCalls++;
        if (restartCalls == 1) {
          throw PlatformException(
            code: 'ICON_RESTART_FAILED',
            message: 'test failure',
          );
        }
        restarted = true;
        current = pending ?? current;
        pending = null;
        return true;
      }
      return null;
    });
    addTearDown(
      () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        channel,
        null,
      ),
    );

    final store = PadStore()..theme = 'light';
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pumpAndSettle();
    expect(pending, 'black');
    tester.platformDispatcher.platformBrightnessTestValue = Brightness.light;
    await tester.pumpAndSettle();
    expect(pending, isNull);
    tester.platformDispatcher.platformBrightnessTestValue = Brightness.dark;
    await tester.pumpAndSettle();
    expect(pending, 'black');

    await tester.tap(find.byTooltip('设置'));
    await tester.pumpAndSettle();
    expect(find.text('桌面图标'), findsOneWidget);
    expect(find.byKey(const ValueKey('app-icon-restart-hint')), findsOneWidget);
    expect(find.text('立即重启'), findsOneWidget);
    expect(restarted, isFalse);
    final hint = tester.widget<Text>(
      find.byKey(const ValueKey('app-icon-restart-hint')),
    );
    final red = (hint.textSpan! as TextSpan).children!.first as TextSpan;
    expect(red.text, '等下次重新启动软件时才会变更');
    expect(red.style?.color, Colors.red.shade700);
    expect(
      tester.widget<Text>(find.text('立即重启')).style?.color,
      Colors.blue.shade700,
    );
    expect(
      tester.widget<Text>(find.text('立即重启')).style?.decoration,
      TextDecoration.underline,
    );

    tester.platformDispatcher.platformBrightnessTestValue = Brightness.light;
    await tester.pumpAndSettle();
    expect(pending, isNull);
    expect(find.byKey(const ValueKey('app-icon-restart-hint')), findsNothing);
    tester.platformDispatcher.platformBrightnessTestValue = Brightness.dark;
    await tester.pumpAndSettle();
    expect(pending, 'black');
    expect(find.byKey(const ValueKey('app-icon-restart-hint')), findsOneWidget);

    await tester.ensureVisible(find.text('默认浅色'));
    await tester.tap(find.text('默认浅色'));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('app-icon-restart-hint')), findsNothing);
    expect(store.appIcon, 'white');

    await tester.ensureVisible(find.text('沉稳深色'));
    await tester.tap(find.text('沉稳深色'));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('app-icon-restart-hint')), findsOneWidget);
    expect(pending, 'black');

    await tester.ensureVisible(find.byKey(const ValueKey('app-icon-restart')));
    await tester.tap(find.byKey(const ValueKey('app-icon-restart')));
    await tester.pump();
    expect(restarted, isFalse);
    expect(pending, 'black');
    expect(find.textContaining('图标变更失败'), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('app-icon-restart')));
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('app-icon-restart')));
    await tester.pump();
    expect(restartCalls, 2);
    expect(restarted, isTrue);
    await tester.pumpWidget(const SizedBox.shrink());
  });
}
