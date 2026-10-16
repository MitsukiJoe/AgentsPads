import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'package:agentpad/app.dart';
import 'package:agentpad/protocol.dart';
import 'package:agentpad/store.dart';

const _fakeNonce = 'fake-nonce';
final _fakeLinks = <String, Device>{};

void _fakeEvent(Object? id, Map<String, Object?> msg) {
  scheduleMicrotask(() {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .handlePlatformMessage(
          'agentpad/ws_events',
          const StandardMethodCodec().encodeSuccessEnvelope({
            'id': id,
            'event': 'text',
            'data': jsonEncode(msg),
          }),
          (_) {},
        );
  });
}

/// Fake desktop: challenge on connect, `connected` once the hello HMAC checks out.
void fakePc(MethodCall call, List<Device> devices) {
  final args = call.arguments;
  if (args is! Map) return;
  final id = args['id'];
  if (call.method == 'connect') {
    final pc = devices.firstWhere((d) => d.ips.contains(args['host']));
    _fakeLinks[id as String] = pc;
    _fakeEvent(id, {
      'type': 'challenge',
      'nonce': _fakeNonce,
      'device_id': pc.deviceId,
    });
  } else if (call.method == 'send') {
    final pc = _fakeLinks[id];
    final msg = jsonDecode(args['text'] as String) as Map;
    if (pc == null || msg['type'] != 'hello') return;
    expect(msg['auth'], authTag(pc.secret, _fakeNonce));
    _fakeEvent(id, {'type': 'connected', 'device_id': pc.deviceId});
  }
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  testWidgets(
    'visibility stops connections and stale connects cannot close resumed links',
    (tester) async {
      SharedPreferences.setMockInitialValues({});
      final first = Completer<bool>();
      final connects = <String>[];
      final closed = <String>[];
      final store = PadStore()
        ..devices = [
          Device(
            deviceId: 'pc',
            name: 'PC',
            ips: ['127.0.0.1'],
            port: 9618,
            secret: 'k',
          ),
        ];
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      messenger.setMockMethodCallHandler(
        const MethodChannel('agentpad/ws_events'),
        (_) async => null,
      );
      messenger.setMockMethodCallHandler(const MethodChannel('agentpad/ws'), (
        call,
      ) async {
        fakePc(call, store.devices);
        if (call.method == 'connect') {
          connects.add((call.arguments as Map)['id'] as String);
          return connects.length == 1 ? first.future : true;
        }
        if (call.method == 'close') {
          closed.add((call.arguments as Map)['id'] as String);
        }
        return call.method == 'send' ? true : null;
      });
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
      await tester.pumpWidget(
        MaterialApp(
          home: HomePage(store: store, enableAutomaticUpdateChecks: false),
        ),
      );
      await tester.pump();
      await tester.enterText(find.byKey(const ValueKey('text-input')), 'draft');
      expect(connects, hasLength(1));
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
      await tester.pump();
      expect(closed, isEmpty);
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.hidden);
      await tester.pump(const Duration(seconds: 5));
      expect(connects, hasLength(1));
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
      await tester.pump();
      expect(connects, hasLength(2));
      expect(connects.toSet(), hasLength(2));
      first.complete(true);
      await tester.pump();
      expect(closed, contains(connects.first));
      expect(closed, isNot(contains(connects.last)));
      expect(find.text('draft'), findsOneWidget);
      expect(store.devices.single.selected, isTrue);
      await tester.tap(find.byKey(const ValueKey('send-button')));
      await tester.pump();
      expect(find.text('draft'), findsNothing);
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.hidden);
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.paused);
      await tester.pump(const Duration(seconds: 5));
      expect(closed, contains(connects.last));
      expect(connects, hasLength(2));
      await tester.pumpWidget(const SizedBox());
      await tester.pump();
      messenger.setMockMethodCallHandler(
        const MethodChannel('agentpad/ws'),
        null,
      );
      messenger.setMockMethodCallHandler(
        const MethodChannel('agentpad/ws_events'),
        null,
      );
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.hidden);
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
      tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    },
  );

  testWidgets('layout has top status and send beside input', (tester) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();
    expect(find.text('发送'), findsNothing);
    final send = tester.widget<FilledButton>(
      find.byKey(const ValueKey('send-button')),
    );
    expect(send.style?.backgroundColor?.resolve({}), const Color(0xFF5EAFF9));
    expect(send.style?.foregroundColor?.resolve({}), Colors.white);
    // Empty draft: the send key is an Enter key; typing flips it to send.
    expect(find.byTooltip('回车'), findsOneWidget);
    expect(find.byIcon(Icons.send_rounded), findsNothing);
    await tester.enterText(find.byKey(const ValueKey('text-input')), 'x');
    await tester.pump();
    expect(find.byTooltip('发送'), findsOneWidget);
    // Icon swap is animated: both glyphs coexist mid-transition.
    expect(find.byIcon(Icons.send_rounded), findsOneWidget);
    expect(find.byIcon(Icons.keyboard_return), findsNWidgets(3));
    await tester.pump(const Duration(milliseconds: 200));
    expect(find.byIcon(Icons.keyboard_return), findsNWidgets(2));
    await tester.enterText(find.byKey(const ValueKey('text-input')), '');
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 200));
    expect(find.byIcon(Icons.send_rounded), findsNothing);
    expect(find.byIcon(Icons.open_in_full), findsOneWidget);
    final input = tester.widget<TextField>(
      find.byKey(const ValueKey('text-input')),
    );
    expect(
      (input.decoration?.enabledBorder as OutlineInputBorder?)
          ?.borderSide
          .color,
      const Color(0xFF5EAFF9),
    );
    expect(find.byKey(const ValueKey('input-height')), findsOneWidget);
    expect(
      tester.getCenter(find.byKey(const ValueKey('send-button'))).dx,
      greaterThan(
        tester.getCenter(find.byKey(const ValueKey('text-input'))).dx,
      ),
    );
    expect(
      tester.getCenter(find.byKey(const ValueKey('send-button'))).dy,
      greaterThan(
        tester.getCenter(find.byKey(const ValueKey('input-height'))).dy,
      ),
    );
    expect(store.inputHeight, 'medium');
    final mediumH = tester
        .getSize(find.byKey(const ValueKey('text-input')))
        .height;
    final heightBtn = tester.getSize(
      find.byKey(const ValueKey('input-height')),
    );
    expect(heightBtn.width, heightBtn.height);
    await tester.enterText(
      find.byKey(const ValueKey('text-input')),
      List.filled(40, '多行文字撑高测试').join('\n'),
    );
    await tester.pump();
    expect(
      tester.getSize(find.byKey(const ValueKey('text-input'))).height,
      mediumH,
    );
    await tester.tap(find.byKey(const ValueKey('input-height')));
    await tester.pump();
    expect(store.inputHeight, 'tall');
    expect(
      tester.getSize(find.byKey(const ValueKey('text-input'))).height,
      greaterThan(mediumH),
    );
    expect(
      tester.getSize(find.byKey(const ValueKey('input-height'))),
      heightBtn,
    );
    expect(
      tester.getSize(find.byKey(const ValueKey('send-button'))).height,
      greaterThan(heightBtn.height),
    );
    await tester.tap(find.byKey(const ValueKey('input-height')));
    await tester.pump();
    expect(store.inputHeight, 'huge');
    final hugeH = tester
        .getSize(find.byKey(const ValueKey('text-input')))
        .height;
    expect(hugeH, greaterThan(mediumH + 50));
    expect(
      tester.getSize(find.byKey(const ValueKey('input-height'))),
      heightBtn,
    );
    await tester.tap(find.byKey(const ValueKey('input-height')));
    await tester.pump();
    expect(store.inputHeight, 'medium');
    expect(
      tester.getSize(find.byKey(const ValueKey('text-input'))).height,
      mediumH,
    );
    expect(find.text('未连接'), findsOneWidget);
    expect(
      tester.getTopLeft(find.byKey(const ValueKey('connection-status'))).dy,
      lessThan(tester.getTopLeft(find.byType(TextField)).dy),
    );
    expect(find.text('撤回上次输入'), findsOneWidget);
    expect(find.text('电脑自动回车'), findsOneWidget);
    expect(find.textContaining('语音自动发送'), findsOneWidget);
    expect(find.textContaining('点击开关发送'), findsNothing);
    expect(find.text('Esc'), findsOneWidget);
    expect(find.text('Enter'), findsNothing);
    expect(find.text('Shift+Enter'), findsNothing);
    // Box still holds the multi-line draft: only the two shortcut chips.
    await tester.pump(const Duration(milliseconds: 200));
    expect(find.byIcon(Icons.keyboard_return), findsNWidgets(2));
    expect(find.byIcon(Icons.send_rounded), findsOneWidget);
    expect(find.text('触控板'), findsOneWidget);
    expect(find.text('轨迹球'), findsOneWidget);
    expect(find.text('小红点'), findsOneWidget);
    expect(find.byKey(const ValueKey('home-pointer-mode')), findsOneWidget);
  });

  testWidgets(
    'home pointer quick switch hides mode bar but settings sync remains',
    (tester) async {
      SharedPreferences.setMockInitialValues({});
      final store = PadStore();
      await store.load();
      await tester.pumpWidget(
        AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
      );
      expect(find.byKey(const ValueKey('home-pointer-mode')), findsOneWidget);

      await tester.tap(find.byTooltip('设置'));
      await tester.pumpAndSettle();
      expect(find.text('跟外面同步'), findsNothing);
      expect(
        find.byKey(const ValueKey('settings-pointer-mode-sync')),
        findsOneWidget,
      );
      final sync = find.byKey(const ValueKey('settings-pointer-mode-sync'));
      await tester.ensureVisible(sync);
      await tester.tap(find.descendant(of: sync, matching: find.text('轨迹球')));
      await tester.pumpAndSettle();
      expect(store.pointerMode, 'trackball');

      final quick = find.byKey(const ValueKey('home-pointer-quick-switch'));
      await tester.ensureVisible(quick);
      // Sticky close button can cover the top of the scroll body.
      tester.widget<Switch>(quick).onChanged!(false);
      await tester.pumpAndSettle();
      expect(store.homePointerQuickSwitch, isFalse);
      expect(
        find.byKey(const ValueKey('settings-pointer-mode-sync')),
        findsOneWidget,
      );
      expect(find.text('切换光标设备'), findsOneWidget);
      expect(find.text('首页显示'), findsOneWidget);

      await tester.tap(find.byKey(const ValueKey('settings-close')));
      await tester.pumpAndSettle();
      expect(find.byKey(const ValueKey('home-pointer-mode')), findsNothing);
      expect(find.byKey(const ValueKey('nub-panel')), findsOneWidget);
    },
  );

  testWidgets('settings offers wheel and device strip controls', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.tap(find.byTooltip('设置'));
    await tester.pumpAndSettle();
    expect(find.text('滚轮位置'), findsOneWidget);
    expect(find.text('滚轮方向反转'), findsOneWidget);
    expect(find.text('已连接设备的快捷开关显示在什么地方'), findsOneWidget);
    expect(find.text('输入框下方'), findsOneWidget);
    expect(find.text('顶部'), findsOneWidget);
    expect(find.text('左侧'), findsOneWidget);
    expect(find.text('右侧'), findsOneWidget);
    expect(find.text('切换光标设备'), findsOneWidget);
    expect(find.text('首页显示'), findsOneWidget);
    expect(find.text('首页快速切换光标设备'), findsNothing);
    expect(find.text('跟外面同步'), findsNothing);
    expect(
      find.byKey(const ValueKey('home-pointer-quick-switch')),
      findsOneWidget,
    );
    expect(
      find.byKey(const ValueKey('settings-pointer-mode-sync')),
      findsOneWidget,
    );
    expect(find.text('触控板大小'), findsOneWidget);
    expect(find.text('小'), findsOneWidget);
    expect(find.text('中'), findsOneWidget);
    expect(find.text('大'), findsOneWidget);
    expect(find.text('指针速度'), findsOneWidget);
    expect(find.text('滚轮速度'), findsOneWidget);
    expect(find.text('×1'), findsWidgets);
    expect(find.text('×2'), findsWidgets);
    expect(find.text('×4'), findsWidgets);
    expect(find.text('×16'), findsWidgets);
    expect(find.text('×7'), findsWidgets);
    expect(find.text('×28'), findsWidgets);
    expect(find.text('×9'), findsNothing);
    expect(store.pointerSpeedWindows, 3);
    expect(store.pointerSpeedMac, 3);
    expect(store.wheelSpeedWindows, 1);
    expect(store.wheelSpeedMac, 16);
    expect(store.wheelReverseWindows, isFalse);
    expect(store.wheelReverseMac, isFalse);
    await tester.ensureVisible(
      find.byKey(const ValueKey('wheel-reverse-windows')),
    );
    tester
        .widget<Switch>(
          find.descendant(
            of: find.byKey(const ValueKey('wheel-reverse-windows')),
            matching: find.byType(Switch),
          ),
        )
        .onChanged!(true);
    await tester.pumpAndSettle();
    expect(store.wheelReverseWindows, isTrue);
    expect(find.text('指针发送频率'), findsOneWidget);
    expect(find.text('60Hz'), findsOneWidget);
    expect(find.text('120Hz'), findsOneWidget);
    expect(find.text('240Hz'), findsOneWidget);
    expect(store.pointerHz, 60);
    expect(find.text('语音自动发送延迟'), findsOneWidget);
    expect(find.text('横屏布局'), findsOneWidget);
    expect(find.text('强制横屏'), findsOneWidget);
    expect(find.byKey(const ValueKey('force-landscape')), findsOneWidget);
    // Settings order after theme: device strip → voice → pointer mode →
    // pad size → wheel → speeds → hz → landscape → force landscape.
    Future<void> expectAbove(String upper, String lower) async {
      await tester.ensureVisible(find.text(upper));
      await tester.ensureVisible(find.text(lower));
      expect(
        tester.getTopLeft(find.text(upper)).dy,
        lessThan(tester.getTopLeft(find.text(lower)).dy),
      );
    }

    await expectAbove('已连接设备的快捷开关显示在什么地方', '语音自动发送延迟');
    await expectAbove('语音自动发送延迟', '切换光标设备');
    await expectAbove('切换光标设备', '触控板大小');
    await expectAbove('触控板大小', '滚轮位置');
    await expectAbove('滚轮位置', '滚轮方向反转');
    await expectAbove('滚轮方向反转', '指针速度');
    await expectAbove('指针速度', '滚轮速度');
    await expectAbove('滚轮速度', '指针发送频率');
    await expectAbove('指针发送频率', '横屏布局');
    await expectAbove('横屏布局', '强制横屏');
    await tester.dragUntilVisible(
      find.byKey(const ValueKey('pointer-speed-windows')),
      find.byKey(const ValueKey('settings-scroll')),
      const Offset(0, -80),
    );
    tester
        .widget<Slider>(
          find.descendant(
            of: find.byKey(const ValueKey('pointer-speed-windows')),
            matching: find.byType(Slider),
          ),
        )
        .onChanged!(4);
    await tester.pumpAndSettle();
    expect(store.pointerSpeedWindows, 5);
    expect(store.pointerSpeedMac, 3);
    await tester.dragUntilVisible(
      find.byKey(const ValueKey('wheel-speed-windows')),
      find.byKey(const ValueKey('settings-scroll')),
      const Offset(0, -80),
    );
    tester
        .widget<Slider>(
          find.descendant(
            of: find.byKey(const ValueKey('wheel-speed-windows')),
            matching: find.byType(Slider),
          ),
        )
        .onChanged!(6);
    await tester.pumpAndSettle();
    expect(store.wheelSpeedWindows, 7);
    await tester.dragUntilVisible(
      find.byKey(const ValueKey('pointer-hz')),
      find.byKey(const ValueKey('settings-scroll')),
      const Offset(0, -80),
    );
    await tester.tap(find.text('120Hz'));
    await tester.pumpAndSettle();
    expect(store.pointerHz, 120);

    await tester.dragUntilVisible(
      find.byIcon(Icons.dark_mode),
      find.byKey(const ValueKey('settings-scroll')),
      const Offset(0, 80),
    );
    await tester.tap(find.byIcon(Icons.dark_mode));
    await tester.pumpAndSettle();
    expect(store.theme, 'dark');
    expect(find.text('外观'), findsOneWidget);
  });

  testWidgets('settings close stays fixed on a short screen', (tester) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    tester.view.physicalSize = const Size(1080, 1200);
    tester.view.display.size = const Size(1080, 2400);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.display.resetSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.tap(find.byTooltip('设置'));
    await tester.pumpAndSettle();

    final close = find.byKey(const ValueKey('settings-close'));
    expect(close, findsOneWidget);
    final position = tester.getTopRight(close);
    await tester.drag(
      find.byKey(const ValueKey('settings-scroll')),
      const Offset(0, -300),
    );
    await tester.pumpAndSettle();
    expect(tester.getTopRight(close), position);
    await tester.tap(close);
    await tester.pumpAndSettle();
    expect(close, findsNothing);
    expect(tester.takeException(), isNull);
  });

  testWidgets('shortcuts sit below pointer and use keyboard symbols', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );

    final pointerBottom = tester
        .getBottomLeft(find.byKey(const ValueKey('pointer-area')))
        .dy;
    final shortcutsTop = tester
        .getTopLeft(find.byKey(const ValueKey('shortcut-section')))
        .dy;
    expect(shortcutsTop, greaterThanOrEqualTo(pointerBottom));
    expect(find.byIcon(Icons.keyboard_return), findsNWidgets(3));
    expect(find.byIcon(Icons.keyboard_capslock), findsOneWidget);
    expect(find.byTooltip('Enter'), findsOneWidget);
    expect(find.byTooltip('Shift+Enter'), findsOneWidget);
    expect(find.text('Esc'), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('shortcut-title')));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('shortcut-preview')), findsOneWidget);
    expect(find.text('快捷键图标预览'), findsOneWidget);
    expect(find.text('临时测试'), findsOneWidget);
    expect(
      find.byKey(const ValueKey('preview-icon-keyboard_return')),
      findsOneWidget,
    );
    expect(
      find.byKey(const ValueKey('preview-icon-keyboard_command_key')),
      findsOneWidget,
    );
    expect(
      find.byKey(const ValueKey('preview-icon-space_bar')),
      findsOneWidget,
    );
    for (final name in const [
      'abc',
      'alternate_email',
      'backspace',
      'calculate',
      'dialpad',
      'functions',
      'key',
      'key_off',
      'numbers',
      'password',
      'pin',
      'subdirectory_arrow_left',
      'tag',
      'text_fields',
      'translate',
    ]) {
      expect(find.byKey(ValueKey('preview-icon-$name')), findsOneWidget);
    }
    final previewDecoration =
        tester
                .widget<Container>(
                  find.byKey(const ValueKey('preview-icon-abc')),
                )
                .decoration
            as BoxDecoration;
    expect(previewDecoration.color, const Color(0xFFD1D0CC));
    expect(previewDecoration.border?.top.color, const Color(0xFFBCBAB7));
    expect(
      tester.widget<Text>(find.text('abc')).style?.color,
      const Color(0xFF868686),
    );
    expect(
      tester.widget<Icon>(find.byIcon(Icons.abc)).color,
      const Color(0xFF868686),
    );
  });

  testWidgets(
    'settings offers ordered theme colors without tinting app theme',
    (tester) async {
      SharedPreferences.setMockInitialValues({});
      final store = PadStore();
      await store.load();
      await tester.pumpWidget(
        AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
      );
      await tester.tap(find.byTooltip('设置'));
      await tester.pumpAndSettle();

      const colors = ['monochrome', 'blue', 'pink', 'green', 'gold', 'red'];
      final centers = <double>[];
      for (final color in colors) {
        final finder = find.byKey(ValueKey('theme-color-$color'));
        expect(finder, findsOneWidget);
        centers.add(tester.getCenter(finder).dx);
      }
      for (var i = 1; i < centers.length; i++) {
        expect(centers[i], greaterThan(centers[i - 1]));
      }
      expect(
        find.byKey(const ValueKey('theme-color-monochrome-split')),
        findsOneWidget,
      );
      final monochromeInner = tester.widget<ClipRRect>(
        find.byKey(const ValueKey('theme-color-monochrome-inner')),
      );
      expect(monochromeInner.borderRadius, BorderRadius.circular(7));
      final selectedMode = tester.widget<IconButton>(
        find.widgetWithIcon(IconButton, Icons.brightness_auto),
      );
      expect(
        selectedMode.style?.backgroundColor?.resolve({}),
        const Color(0xFFF0F0F0),
      );
      expect(
        selectedMode.style?.foregroundColor?.resolve({}),
        const Color(0xFF141414),
      );
      BoxDecoration swatch(String name) =>
          tester
                  .widget<AnimatedContainer>(
                    find.descendant(
                      of: find.byKey(ValueKey('theme-color-$name')),
                      matching: find.byType(AnimatedContainer),
                    ),
                  )
                  .decoration
              as BoxDecoration;
      expect(swatch('blue').color, const Color(0xFF5EAFF9));
      expect(swatch('green').color, const Color(0xFF74B580));
      expect(swatch('pink').color, const Color(0xFFE9ACB7));
      expect(swatch('blue').boxShadow, isNull);
      expect(swatch('blue').border?.top.width, 1);
      expect(swatch('green').border?.top.color, const Color(0xFFBCBAB7));
      expect(store.themeColor, 'blue');
      final blueApp = tester.widget<MaterialApp>(find.byType(MaterialApp));
      expect(blueApp.theme?.colorScheme.primary, const Color(0xFF333333));
      expect(blueApp.darkTheme?.colorScheme.primary, const Color(0xFFCCCCCC));
      await tester.tap(find.byKey(const ValueKey('theme-color-green')));
      await tester.pumpAndSettle();
      expect(store.themeColor, 'green');
      final greenApp = tester.widget<MaterialApp>(find.byType(MaterialApp));
      expect(greenApp.theme?.colorScheme.surface, const Color(0xFFF0F0F0));
      expect(greenApp.darkTheme?.colorScheme.surface, const Color(0xFF141414));
      expect(greenApp.theme?.colorScheme, blueApp.theme?.colorScheme);
      expect(greenApp.darkTheme?.colorScheme, blueApp.darkTheme?.colorScheme);
      final send = tester.widget<FilledButton>(
        find.byKey(const ValueKey('send-button')),
      );
      expect(send.style?.backgroundColor?.resolve({}), const Color(0xFF74B580));
      expect(send.style?.foregroundColor?.resolve({}), Colors.white);
    },
  );

  testWidgets('bordered controls use fixed state colors and raw accents', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    final app = tester.widget<MaterialApp>(find.byType(MaterialApp));
    final light = app.theme!;
    final dark = app.darkTheme!;
    final lightStyle = light.segmentedButtonTheme.style!;
    final darkStyle = dark.segmentedButtonTheme.style!;
    expect(lightStyle.backgroundColor?.resolve({}), const Color(0xFFD1D0CC));
    expect(darkStyle.backgroundColor?.resolve({}), const Color(0xFF2C2B28));
    expect(lightStyle.foregroundColor?.resolve({}), const Color(0xFF868686));
    expect(darkStyle.foregroundColor?.resolve({}), const Color(0xFF6B6B6B));
    expect(lightStyle.side?.resolve({})?.color, const Color(0xFFBCBAB7));
    expect(darkStyle.side?.resolve({})?.color, const Color(0xFF3D3C39));
    expect(
      lightStyle.side?.resolve({WidgetState.selected})?.color,
      const Color(0xFFBCBAB7),
    );
    expect(
      lightStyle.backgroundColor?.resolve({WidgetState.selected}),
      const Color(0xFFF0F0F0),
    );
    expect(
      lightStyle.foregroundColor?.resolve({WidgetState.selected}),
      const Color(0xFF141414),
    );
    expect(lightStyle.side?.resolve({WidgetState.selected})?.width, 1);
    expect(light.chipTheme.backgroundColor, const Color(0xFFD1D0CC));
    expect(light.chipTheme.side?.color, const Color(0xFFBCBAB7));
    expect(light.chipTheme.labelStyle?.color, const Color(0xFF868686));
    expect(dark.chipTheme.backgroundColor, const Color(0xFF2C2B28));
    expect(dark.chipTheme.side?.color, const Color(0xFF3D3C39));
    expect(dark.chipTheme.labelStyle?.color, const Color(0xFF6B6B6B));
  });

  testWidgets('theme colors keep one neutral light and dark color scheme', (
    tester,
  ) async {
    ColorScheme? expectedLight;
    ColorScheme? expectedDark;
    for (final color in const [
      'blue',
      'monochrome',
      'green',
      'pink',
      'gold',
      'red',
    ]) {
      SharedPreferences.setMockInitialValues({'theme_color': color});
      final store = PadStore();
      await store.load();
      await tester.pumpWidget(
        AgentsPadsApp(
          key: ValueKey(color),
          store: store,
          enableAutomaticUpdateChecks: false,
        ),
      );
      final app = tester.widget<MaterialApp>(find.byType(MaterialApp));
      expectedLight ??= app.theme?.colorScheme;
      expectedDark ??= app.darkTheme?.colorScheme;
      expect(app.theme?.colorScheme, expectedLight);
      expect(app.darkTheme?.colorScheme, expectedDark);
      expect(app.theme?.colorScheme.surface, const Color(0xFFF0F0F0));
      expect(app.theme?.scaffoldBackgroundColor, const Color(0xFFF0F0F0));
      expect(
        {
          app.theme?.colorScheme.surfaceDim,
          app.theme?.colorScheme.surfaceBright,
          app.theme?.colorScheme.surfaceContainerLowest,
          app.theme?.colorScheme.surfaceContainerLow,
          app.theme?.colorScheme.surfaceContainer,
          app.theme?.colorScheme.surfaceContainerHigh,
          app.theme?.colorScheme.surfaceContainerHighest,
        },
        {const Color(0xFFF0F0F0)},
      );
      expect(app.theme?.colorScheme.surfaceTint, Colors.transparent);
      expect(app.theme?.colorScheme.primary, const Color(0xFF333333));
      expect(app.theme?.colorScheme.onPrimary, Colors.white);
      expect(app.darkTheme?.colorScheme.surface, const Color(0xFF141414));
      expect(app.darkTheme?.scaffoldBackgroundColor, const Color(0xFF141414));
      expect(
        {
          app.darkTheme?.colorScheme.surfaceDim,
          app.darkTheme?.colorScheme.surfaceBright,
          app.darkTheme?.colorScheme.surfaceContainerLowest,
          app.darkTheme?.colorScheme.surfaceContainerLow,
          app.darkTheme?.colorScheme.surfaceContainer,
          app.darkTheme?.colorScheme.surfaceContainerHigh,
          app.darkTheme?.colorScheme.surfaceContainerHighest,
        },
        {const Color(0xFF141414)},
      );
      expect(app.darkTheme?.colorScheme.surfaceTint, Colors.transparent);
      expect(app.darkTheme?.colorScheme.primary, const Color(0xFFCCCCCC));
      expect(app.darkTheme?.colorScheme.onPrimary, Colors.black);
    }
  });

  testWidgets('monochrome dark send and input text use contrasting neutrals', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({
      'theme': 'dark',
      'theme_color': 'monochrome',
    });
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    final app = tester.widget<MaterialApp>(find.byType(MaterialApp));
    final send = tester.widget<FilledButton>(
      find.byKey(const ValueKey('send-button')),
    );
    expect(send.style?.backgroundColor?.resolve({}), const Color(0xFFCCCCCC));
    expect(send.style?.foregroundColor?.resolve({}), Colors.black);
    await tester.enterText(find.byType(TextField).first, 'neutral');
    await tester.pump();
    final input = tester.widget<EditableText>(find.byType(EditableText).first);
    expect(input.style.color, app.darkTheme?.colorScheme.onSurface);
    final textColor = input.style.color!;
    expect(textColor.r, closeTo(textColor.g, 0.001));
    expect(textColor.g, closeTo(textColor.b, 0.001));
  });

  testWidgets('colored themes use white send icon', (tester) async {
    SharedPreferences.setMockInitialValues({
      'theme': 'light',
      'theme_color': 'red',
    });
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    final redSend = tester.widget<FilledButton>(
      find.byKey(const ValueKey('send-button')),
    );
    expect(redSend.style?.foregroundColor?.resolve({}), Colors.white);

    store.themeColor = 'blue';
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();
    final blueSend = tester.widget<FilledButton>(
      find.byKey(const ValueKey('send-button')),
    );
    expect(blueSend.style?.foregroundColor?.resolve({}), Colors.white);
  });

  testWidgets('trackpad size leaves nub compact', (tester) async {
    SharedPreferences.setMockInitialValues({'pointer_size': 'large'});
    final store = PadStore();
    await store.load();
    tester.view.physicalSize = const Size(1080, 2400);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    addTearDown(tester.view.resetViewInsets);
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    expect(
      tester.getSize(find.byKey(const ValueKey('pointer-area'))).height,
      240,
    );

    tester.view.viewInsets = const FakeViewPadding(bottom: 420);
    await tester.pump();
    expect(
      tester.getSize(find.byKey(const ValueKey('pointer-area'))).height,
      240,
    );

    await tester.ensureVisible(find.text('轨迹球'));
    await tester.tap(find.text('轨迹球'));
    await tester.pump();
    final ballH = tester
        .getSize(find.byKey(const ValueKey('pointer-area')))
        .height;
    expect(ballH, lessThan(140));
    expect(ballH, greaterThan(80));
    expect(
      tester.getSize(find.byKey(const ValueKey('pointer-cap'))),
      const Size.square(60),
    );

    store.pointerSize = 'small';
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();
    await tester.tap(find.text('轨迹球'));
    await tester.pump();
    expect(
      tester.getSize(find.byKey(const ValueKey('pointer-area'))).height,
      ballH,
    );

    await tester.tap(find.text('小红点'));
    await tester.pump();
    expect(
      tester.getSize(find.byKey(const ValueKey('pointer-area'))).height,
      ballH,
    );
    expect(
      tester.getSize(find.byKey(const ValueKey('pointer-cap'))),
      const Size.square(60),
    );

    await tester.tap(find.text('触控板'));
    await tester.pump();
    expect(
      tester.getSize(find.byKey(const ValueKey('pointer-area'))).height,
      104,
    );
  });

  testWidgets('pointer press temporarily disables page scrolling', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    SingleChildScrollView page() =>
        tester.widget(find.byKey(const ValueKey('page-scroll')));
    expect(page().physics, isNot(isA<NeverScrollableScrollPhysics>()));
    final gesture = await tester.startGesture(
      tester.getCenter(find.byKey(const ValueKey('pointer-input'))),
    );
    await tester.pump();
    expect(page().physics, isA<NeverScrollableScrollPhysics>());
    await gesture.up();
    await tester.pump();
    expect(page().physics, isNot(isA<NeverScrollableScrollPhysics>()));
  });

  testWidgets('pointer transport does not schedule rendering frames', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    final gesture = await tester.startGesture(
      tester.getCenter(find.byKey(const ValueKey('pointer-input'))),
    );
    await tester.pump();
    await gesture.moveBy(const Offset(20, 0));
    expect(tester.binding.transientCallbackCount, 0);
    await gesture.up();
  });

  testWidgets(
    'target switch releases old buttons and discards an ongoing trackball gesture',
    (tester) async {
      SharedPreferences.setMockInitialValues({});
      final store = PadStore()
        ..pointerMode = 'trackball'
        ..devices = [
          for (final id in ['a', 'b'])
            Device(
              deviceId: id,
              name: id,
              ips: [id],
              secret: 'k',
              port: 9618,
              selected: id == 'a',
            ),
        ];
      const ws = MethodChannel('agentpad/ws');
      const events = MethodChannel('agentpad/ws_events');
      final calls = <Map>[];
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(ws, (
        call,
      ) async {
        fakePc(call, store.devices);
        if (call.method == 'pointer') calls.add(call.arguments as Map);
        return ['connect', 'send', 'pointer', 'close'].contains(call.method)
            ? true
            : null;
      });
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        events,
        (_) async => null,
      );
      addTearDown(() {
        tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
          ws,
          null,
        );
        tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
          events,
          null,
        );
      });
      await tester.pumpWidget(
        AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
      );
      await tester.pump();
      await tester.pump();
      final gesture = await tester.startGesture(
        tester.getCenter(find.byKey(const ValueKey('pointer-cap'))),
      );
      await gesture.moveBy(const Offset(25, 0));
      await tester.pump(const Duration(milliseconds: 20));
      store.selectDevice(store.devices.last, true);
      await tester.pump();
      expect(calls.last['id'], startsWith('a:'));
      expect(calls.last['buttons'], 0);
      final count = calls.length;
      await gesture.moveBy(const Offset(25, 0));
      await tester.pump(const Duration(milliseconds: 25));
      expect(calls, hasLength(count));
      await gesture.up();
      await tester.pump();
      final next = await tester.startGesture(
        tester.getCenter(find.byKey(const ValueKey('pointer-cap'))),
      );
      await next.moveBy(const Offset(25, 0));
      await next.moveBy(const Offset(10, 0));
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 25)),
      );
      await tester.pump(const Duration(milliseconds: 20));
      expect(calls.last['id'], startsWith('b:'));
      await next.up();
      await tester.pumpWidget(const SizedBox.shrink());
    },
  );

  testWidgets('pointer speed is scaled per connected platform', (tester) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore()
      ..limitActiveDevices = false
      ..pointerSpeedWindows = 2
      ..pointerSpeedMac = 5
      ..devices = [
        Device(
          deviceId: 'win',
          name: 'Win',
          ips: const ['127.0.0.1'],
          secret: 'k',
          port: 9618,
          os: 'Windows 11',
        ),
        Device(
          deviceId: 'mac',
          name: 'Mac',
          ips: const ['127.0.0.2'],
          secret: 'k',
          port: 9618,
          os: 'macOS',
        ),
      ];

    const ws = MethodChannel('agentpad/ws');
    const events = MethodChannel('agentpad/ws_events');
    final pointerCalls = <Map>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(ws, (
      call,
    ) async {
      fakePc(call, store.devices);
      if (call.method == 'connect') return true;
      if (call.method == 'pointer') {
        pointerCalls.add((call.arguments as Map).cast<String, dynamic>());
        return true;
      }
      if (call.method == 'send') return true;
      if (call.method == 'displayRefreshHz') return 60.0;
      if (call.method == 'close') return true;
      if (call.method == 'resetVoiceEvidence') return true;
      return null;
    });
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      events,
      (_) async => null,
    );
    addTearDown(() {
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(ws, null);
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        events,
        null,
      );
    });

    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();
    await tester.pump();

    final gesture = await tester.startGesture(
      tester.getCenter(find.byKey(const ValueKey('pointer-input'))),
    );
    await tester.pump();
    await gesture.moveBy(const Offset(4, 0));
    await tester.pump(const Duration(milliseconds: 20));
    await gesture.up();
    await tester.pump();

    final moves = pointerCalls.where((call) => call['dx'] != 0).toList();
    expect(
      moves,
      containsAll([
        containsPair('id', startsWith('win:')),
        containsPair('id', startsWith('mac:')),
      ]),
    );
    expect(
      moves.firstWhere(
        (call) => (call['id'] as String).startsWith('win:'),
      )['dx'],
      8,
    );
    expect(
      moves.firstWhere(
        (call) => (call['id'] as String).startsWith('mac:'),
      )['dx'],
      20,
    );

    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets(
    'keeps a manual 60Hz after the refresh migration on a high-refresh display',
    (tester) async {
      SharedPreferences.setMockInitialValues({
        'pointer_hz_peak_v1': true,
        'pointer_hz': 60,
        'pointer_hz_manual': true,
      });
      final store = PadStore();
      await store.load();
      expect(store.pointerHz, 60);
      expect(store.pointerHzManual, isTrue);

      const ws = MethodChannel('agentpad/ws');
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(ws, (
        call,
      ) async {
        if (call.method == 'displayRefreshHz') return 120.0;
        return null;
      });
      addTearDown(() {
        tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
          ws,
          null,
        );
      });

      await tester.pumpWidget(
        AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
      );
      await tester.pump();
      await tester.pump();

      expect(store.pointerHz, 60);
      expect(store.pointerHzManual, isTrue);
      final prefs = await SharedPreferences.getInstance();
      expect(prefs.getInt('pointer_hz'), 60);
      expect(prefs.getBool('pointer_hz_manual'), isTrue);

      await tester.pumpWidget(const SizedBox.shrink());
    },
  );

  testWidgets('home modules share one horizontal page gutter', (tester) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    tester.view.physicalSize = const Size(1080, 2400);
    tester.view.devicePixelRatio = 3;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();
    final actionLeft = tester
        .getTopLeft(find.byKey(const ValueKey('action-voice-auto-send')))
        .dx;
    final padLeft = tester
        .getTopLeft(find.byKey(const ValueKey('touchpad-surface')))
        .dx;
    final input = tester.getRect(find.byKey(const ValueKey('text-input')));
    final inputLeft = input.left;
    final wheel = tester.getRect(find.byKey(const ValueKey('touchpad-wheel')));
    final sendButton = tester.getRect(
      find.byKey(const ValueKey('send-button')),
    );
    final modeLeft = tester.getTopLeft(find.text('触控板')).dx;
    expect(actionLeft, 12);
    expect(padLeft, actionLeft);
    expect(inputLeft, actionLeft);
    expect(wheel.width, sendButton.width);
    expect(wheel.right, sendButton.right);
    expect(modeLeft, greaterThanOrEqualTo(actionLeft));
  });

  testWidgets(
    'wheel glyph rides the finger, springs back, reduce-motion freezes it',
    (tester) async {
      SharedPreferences.setMockInitialValues({});
      final store = PadStore();
      await store.load();
      expect(store.reduceMotion, isFalse);
      await tester.pumpWidget(
        AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
      );
      await tester.pump();
      final glyph = find.byIcon(Icons.swap_vert);
      final rest = tester.getCenter(glyph).dy;
      final wheelRect = tester.getRect(
        find.byKey(const ValueKey('touchpad-wheel')),
      );

      var g = await tester.startGesture(wheelRect.center);
      await g.moveBy(const Offset(0, 30));
      await tester.pump();
      expect(tester.getCenter(glyph).dy, rest + 30);
      // Clamped inside the strip.
      await g.moveBy(const Offset(0, 500));
      await tester.pump();
      expect(tester.getCenter(glyph).dy, lessThan(wheelRect.bottom));
      await g.up();
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 130));
      // Quarter way through elasticOut: already past rest (overshoot).
      expect(tester.getCenter(glyph).dy, lessThan(rest));
      await tester.pump(const Duration(milliseconds: 500));
      expect(tester.getCenter(glyph).dy, rest);

      await tester.tap(find.byTooltip('设置'));
      await tester.pumpAndSettle();
      final toggle = find.byKey(const ValueKey('reduce-motion'));
      await tester.scrollUntilVisible(
        toggle,
        200,
        scrollable: find.descendant(
          of: find.byKey(const ValueKey('settings-scroll')),
          matching: find.byType(Scrollable),
        ),
      );
      await tester.tap(toggle);
      await tester.pumpAndSettle();
      expect(store.reduceMotion, isTrue);
      await tester.tap(find.byKey(const ValueKey('settings-close')));
      await tester.pumpAndSettle();

      g = await tester.startGesture(wheelRect.center);
      await g.moveBy(const Offset(0, 30));
      await tester.pump();
      expect(tester.getCenter(glyph).dy, rest);
      await g.up();
      await tester.pump();
    },
  );

  testWidgets('touchpad and flat wheel share fixed neutral styling', (
    tester,
  ) async {
    for (final (mode, background, border, foreground) in [
      (
        'light',
        const Color(0xFFD1D0CC),
        const Color(0xFFBCBAB7),
        const Color(0xFF868686),
      ),
      (
        'dark',
        const Color(0xFF2C2B28),
        const Color(0xFF3D3C39),
        const Color(0xFF6B6B6B),
      ),
    ]) {
      SharedPreferences.setMockInitialValues({'theme': mode});
      final store = PadStore();
      await store.load();
      await tester.pumpWidget(
        AgentsPadsApp(
          key: ValueKey('touchpad-$mode'),
          store: store,
          enableAutomaticUpdateChecks: false,
        ),
      );

      final pad = tester.widget<DecoratedBox>(
        find.byKey(const ValueKey('touchpad-surface')),
      );
      final wheel = tester.widget<Container>(
        find.byKey(const ValueKey('touchpad-wheel')),
      );
      final padDecoration = pad.decoration as BoxDecoration;
      final wheelDecoration = wheel.decoration as BoxDecoration;
      expect(padDecoration.color, background);
      expect(wheelDecoration.color, background);
      expect(padDecoration.border?.top.color, border);
      expect(wheelDecoration.border?.top.color, border);
      expect(padDecoration.border?.top.width, 1);
      expect(wheelDecoration.border?.top.width, 1);
      expect(
        tester
            .widget<Icon>(
              find.descendant(
                of: find.byKey(const ValueKey('touchpad-wheel')),
                matching: find.byIcon(Icons.swap_vert),
              ),
            )
            .color,
        foreground,
      );
    }
  });

  testWidgets('touchpad keeps page locked until both fingers lift', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    SingleChildScrollView page() =>
        tester.widget(find.byKey(const ValueKey('page-scroll')));
    final center = tester.getCenter(
      find.byKey(const ValueKey('pointer-input')),
    );
    final first = await tester.startGesture(center, pointer: 1);
    final second = await tester.startGesture(
      center + const Offset(20, 0),
      pointer: 2,
    );
    await tester.pump();
    expect(page().physics, isA<NeverScrollableScrollPhysics>());
    await first.up();
    await tester.pump();
    expect(page().physics, isA<NeverScrollableScrollPhysics>());
    await second.up();
    await tester.pump();
    expect(page().physics, isNot(isA<NeverScrollableScrollPhysics>()));
    expect(tester.takeException(), isNull);
  });

  testWidgets('touchpad drag does not move the page under the finger', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    tester.view.physicalSize = const Size(360, 420);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();

    final vertical = find.descendant(
      of: find.byKey(const ValueKey('page-scroll')),
      matching: find.byWidgetPredicate(
        (w) => w is Scrollable && w.axis == Axis.vertical,
      ),
    );
    expect(
      tester.state<ScrollableState>(vertical.first).position.maxScrollExtent,
      greaterThan(0),
    );

    final pad = find.byKey(const ValueKey('touchpad-surface'));
    final before = tester.getTopLeft(pad);
    final gesture = await tester.startGesture(tester.getCenter(pad));
    await gesture.moveBy(const Offset(0, -90));
    await tester.pump();
    expect(tester.getTopLeft(pad), before);
    await gesture.up();
  });

  testWidgets('action row uses bordered buttons with selected state', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    expect(find.text('撤回上次输入'), findsOneWidget);
    expect(find.text('电脑自动回车'), findsOneWidget);
    expect(find.text('语音自动发送'), findsOneWidget);
    final voice = find.byKey(const ValueKey('action-voice-auto-send'));
    final enter = find.byKey(const ValueKey('action-auto-enter'));
    final undo = find.byKey(const ValueKey('action-undo'));
    final voiceX = tester.getCenter(voice).dx;
    final enterX = tester.getCenter(enter).dx;
    final undoX = tester.getCenter(undo).dx;
    final actionBottom = tester.getBottomLeft(undo).dy;
    final inputTop = tester.getTopLeft(find.byType(TextField)).dy;
    expect(voiceX, lessThan(enterX));
    expect(enterX, lessThan(undoX));
    expect(actionBottom, lessThanOrEqualTo(inputTop));
    BoxDecoration deco(Finder f) =>
        tester
                .widget<AnimatedContainer>(
                  find.descendant(
                    of: f,
                    matching: find.byType(AnimatedContainer),
                  ),
                )
                .decoration
            as BoxDecoration;
    expect(deco(voice).color, const Color(0xFFF0F0F0));
    expect(deco(voice).border?.top.color, const Color(0xFF5EAFF9));
    expect(deco(voice).border?.top.width, 1);
    expect(deco(voice).borderRadius, BorderRadius.circular(17));
    expect(deco(voice).boxShadow, isNotNull);
    expect(deco(voice).boxShadow!.first.blurRadius, 5);
    expect(deco(voice).boxShadow!.first.offset, Offset.zero);
    expect(
      tester
          .getSize(
            find.descendant(
              of: voice,
              matching: find.byType(AnimatedContainer),
            ),
          )
          .height,
      34,
    );
    expect(deco(enter).color, const Color(0xFFD1D0CC));
    expect(deco(enter).border?.top.color, const Color(0xFFBCBAB7));
    expect(deco(enter).boxShadow, isNull);
    expect(deco(undo).color, const Color(0xFFF0F0F0));
    expect(deco(undo).border?.top.color, const Color(0xFFBCBAB7));
    expect(deco(undo).boxShadow, isNull);
    TextStyle labelStyle(Finder f) => tester
        .widget<Text>(find.descendant(of: f, matching: find.byType(Text)))
        .style!;
    expect(labelStyle(voice).fontWeight, FontWeight.w700);
    expect(labelStyle(voice).color, const Color(0xFF141414));
    expect(labelStyle(enter).fontWeight, FontWeight.w700);
    expect(labelStyle(enter).color, const Color(0xFF868686));
    expect(labelStyle(undo).fontWeight, FontWeight.w700);
    expect(labelStyle(undo).color, const Color(0xFF141414));
    expect(find.text('自动 Enter'), findsNothing);
    await tester.tap(find.text('电脑自动回车'));
    await tester.pump();
    expect(store.autoEnter, isTrue);
  });

  testWidgets('connection status is borderless bold and state colored', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    final button = tester.widget<TextButton>(
      find.byKey(const ValueKey('connection-status')),
    );
    expect(button.style?.side?.resolve({}), isNull);
    expect(button.style?.foregroundColor?.resolve({}), const Color(0xFFE5484D));
    expect(button.style?.textStyle?.resolve({})?.fontWeight, FontWeight.w700);
    final icon = tester.widget<Icon>(
      find.byKey(const ValueKey('connection-icon-disconnected')),
    );
    expect(icon.shadows, isNull);
    expect(
      find.descendant(
        of: find.byKey(const ValueKey('connection-status')),
        matching: find.byType(ImageFiltered),
      ),
      findsOneWidget,
    );
  });

  testWidgets('trackpad explains single and two finger gestures on two lines', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    expect(
      find.text(
        '单指：单击左键 / 长按拖动区选 / 长按松开右键\n'
        '双指：点按右键 / 上下滑动滚轮',
      ),
      findsOneWidget,
    );
  });

  testWidgets('wheel strip sends each half pixel at the Mac multiplier', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore()
      ..devices = [
        Device(
          deviceId: 'mac',
          name: 'Mac',
          ips: ['127.0.0.1'],
          secret: 'k',
          port: 9618,
          os: 'macos',
        ),
      ];
    const ws = MethodChannel('agentpad/ws');
    const events = MethodChannel('agentpad/ws_events');
    final packets = <Map>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(ws, (
      call,
    ) async {
      if (call.method == 'pointer') packets.add(call.arguments as Map);
      fakePc(call, store.devices);
      if (call.method == 'connect' || call.method == 'send') return true;
      return null;
    });
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      events,
      (_) async => null,
    );
    addTearDown(() {
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(ws, null);
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        events,
        null,
      );
    });
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();
    await tester.pump();
    expect(find.text('已连接 1 台'), findsOneWidget);
    final wheel = await tester.startGesture(
      tester.getCenter(find.byKey(const ValueKey('touchpad-wheel'))),
      pointer: 1,
    );
    for (var i = 0; i < 2; i++) {
      await wheel.moveBy(const Offset(0, 0.5));
      final tap = await tester.startGesture(
        tester.getCenter(find.byKey(const ValueKey('touchpad-surface'))),
        pointer: 2,
      );
      await tap.up();
      await tester.pump();
      expect(
        packets.fold<int>(0, (sum, p) => sum + (p['wheel'] as int)),
        -8 * (i + 1),
      );
    }
    await wheel.up();
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('long press haptic fires when armed and honors the setting', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    final haptics = <MethodCall>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      (call) async {
        if (call.method == 'HapticFeedback.vibrate') haptics.add(call);
        return null;
      },
    );
    addTearDown(() {
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        SystemChannels.platform,
        null,
      );
    });
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    for (final enabled in [true, false]) {
      store.longPressHaptic = enabled;
      haptics.clear();
      final gesture = await tester.startGesture(
        tester.getCenter(find.byKey(const ValueKey('touchpad-surface'))),
      );
      await tester.pump(const Duration(milliseconds: 600));
      expect(haptics.length, enabled ? 1 : 0);
      await gesture.up();
      await tester.pump();
    }
    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('long press haptic setting toggles from settings', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    expect(store.longPressHaptic, isTrue);
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );

    await tester.tap(find.byTooltip('设置'));
    await tester.pumpAndSettle();
    await tester.dragUntilVisible(
      find.byKey(const ValueKey('long-press-haptic')),
      find.byKey(const ValueKey('settings-scroll')),
      const Offset(0, -80),
    );
    await tester.drag(
      find.byKey(const ValueKey('settings-scroll')),
      const Offset(0, 120),
    );
    await tester.pumpAndSettle();
    expect(find.text('长按轻震反馈'), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('long-press-haptic')));
    await tester.pump();
    expect(store.longPressHaptic, isFalse);
  });

  testWidgets('voice delay and help live in settings', (tester) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );

    await tester.longPress(find.text('语音自动发送'));
    await tester.pumpAndSettle();
    expect(find.text('语音自动发送说明'), findsNothing);

    await tester.tap(find.byTooltip('设置'));
    await tester.pumpAndSettle();
    await tester.dragUntilVisible(
      find.text('语音自动发送延迟'),
      find.byKey(const ValueKey('settings-scroll')),
      const Offset(0, -80),
    );
    expect(find.text('语音自动发送延迟'), findsOneWidget);
    expect(find.text('无延迟'), findsOneWidget);
    expect(find.text('0.5 秒'), findsOneWidget);
    expect(find.text('1 秒'), findsOneWidget);
    expect(find.text('1.5 秒'), findsOneWidget);
    expect(store.voiceDelayMs, 500);

    await tester.tap(find.byTooltip('语音自动发送说明'));
    await tester.pumpAndSettle();
    expect(find.text('语音自动发送说明'), findsOneWidget);
    expect(find.textContaining('默认等待 0.5 秒'), findsOneWidget);
    expect(find.textContaining('AI 自动整理排版'), findsOneWidget);
    expect(find.textContaining('排列和重组'), findsOneWidget);
    expect(find.textContaining('录音活动'), findsOneWidget);
    expect(find.textContaining('手打'), findsOneWidget);
    expect(find.textContaining('粘贴'), findsOneWidget);
  });

  testWidgets('device management help stays inside the connected sheet', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    expect(find.text('长按设备可删除，拖动可排序'), findsNothing);
    await tester.tap(find.byKey(const ValueKey('connection-status')));
    await tester.pumpAndSettle();
    expect(find.text('长按设备可删除，拖动可排序'), findsOneWidget);
  });

  testWidgets('top connection status shows disconnected and connecting', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final empty = PadStore();
    await empty.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: empty, enableAutomaticUpdateChecks: false),
    );
    expect(find.text('未连接'), findsOneWidget);
    expect(
      find.byKey(const ValueKey('connection-icon-disconnected')),
      findsOneWidget,
    );
    await tester.tap(find.byKey(const ValueKey('connection-status')));
    await tester.pumpAndSettle();
    expect(find.text('已连接'), findsOneWidget);
    await tester.tapAt(const Offset(8, 8));
    await tester.pumpAndSettle();
    await tester.pumpWidget(const SizedBox.shrink());

    final saved = PadStore()
      ..devices = [
        Device(deviceId: 'a', name: 'Mac', ips: const [], port: 9618),
      ];
    await tester.pumpWidget(
      AgentsPadsApp(store: saved, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();
    expect(find.text('连接中'), findsOneWidget);
    expect(
      find.byKey(const ValueKey('connection-icon-connecting')),
      findsOneWidget,
    );
    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump(const Duration(seconds: 2));
  });

  testWidgets('device strip toggles targets and exposes capsule actions', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    tester.view.physicalSize = const Size(600, 800);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    final store = PadStore()
      ..devices = [
        Device(deviceId: 'a', name: 'Mac', ips: const [], port: 9618),
        Device(deviceId: 'b', name: 'PC', ips: const [], port: 9618),
      ];
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();

    final strip = tester.widget<ListView>(
      find.byKey(const ValueKey('device-strip')),
    );
    expect(strip.scrollDirection, Axis.horizontal);
    expect(find.text('Mac'), findsOneWidget);
    expect(find.text('PC'), findsOneWidget);
    expect(
      tester.getTopLeft(find.byKey(const ValueKey('device-strip'))).dy,
      greaterThan(
        tester.getBottomLeft(find.byKey(const ValueKey('text-input'))).dy,
      ),
    );
    final capsule = find.byKey(const ValueKey('device-capsule-a'));
    expect(capsule, findsOneWidget);
    BoxDecoration capsuleDecoration() =>
        tester
                .widget<AnimatedContainer>(
                  find.descendant(
                    of: capsule,
                    matching: find.byType(AnimatedContainer),
                  ),
                )
                .decoration
            as BoxDecoration;
    expect(capsuleDecoration().color, const Color(0xFFF0F0F0));
    expect(capsuleDecoration().border?.top.color, const Color(0xFF5EAFF9));
    expect(capsuleDecoration().border?.top.width, 1);
    expect(
      tester
          .getSize(
            find.descendant(
              of: capsule,
              matching: find.byType(AnimatedContainer),
            ),
          )
          .height,
      34,
    );
    expect(capsuleDecoration().borderRadius, BorderRadius.circular(17));
    expect(capsuleDecoration().boxShadow, isNotNull);
    expect(capsuleDecoration().boxShadow, isNotEmpty);
    expect(
      capsuleDecoration().boxShadow!.first.color,
      const Color(0xFF5EAFF9).withValues(alpha: 0.42),
    );
    expect(capsuleDecoration().boxShadow!.first.blurRadius, 5);
    expect(capsuleDecoration().boxShadow!.first.spreadRadius, 0.25);
    expect(capsuleDecoration().boxShadow!.first.offset, Offset.zero);
    Text deviceName() =>
        tester.widget<Text>(find.byKey(const ValueKey('device-name-a')));
    expect(deviceName().style?.fontSize, 12);
    expect(deviceName().style?.fontWeight, FontWeight.w600);
    expect(deviceName().style?.color, const Color(0xFF141414));

    await tester.tap(capsule);
    await tester.pumpAndSettle();
    expect(store.devices.first.selected, isFalse);
    expect(
      find.descendant(of: capsule, matching: find.byType(AnimatedOpacity)),
      findsNothing,
    );
    expect(capsuleDecoration().color, const Color(0xFFD1D0CC));
    expect(capsuleDecoration().border?.top.color, const Color(0xFFBCBAB7));
    expect(capsuleDecoration().boxShadow, isNull);
    expect(deviceName().style?.color, const Color(0xFF868686));
    final dot = tester.widget<Container>(
      find.byKey(const ValueKey('device-toggle-a')),
    );
    expect((dot.decoration as BoxDecoration).color, const Color(0xFF868686));

    final capsuleRect = tester.getRect(capsule);
    await tester.longPress(capsule);
    await tester.pumpAndSettle();
    final actions = find.byKey(const ValueKey('device-actions'));
    expect(actions, findsOneWidget);
    final actionsRect = tester.getRect(actions);
    expect(actionsRect.size, const Size(132, 44));
    expect(actionsRect.bottom, lessThanOrEqualTo(capsuleRect.top));
    expect(actionsRect.left, greaterThanOrEqualTo(0));
    expect(
      actionsRect.right,
      lessThanOrEqualTo(tester.view.physicalSize.width),
    );
    expect(find.byIcon(Icons.chevron_left), findsOneWidget);
    expect(find.byIcon(Icons.chevron_right), findsOneWidget);
    expect(find.byIcon(Icons.arrow_back), findsNothing);
    expect(find.byIcon(Icons.arrow_forward), findsNothing);
    expect(find.byIcon(Icons.delete_outline), findsOneWidget);
    final iconTheme = tester.widget<IconButtonTheme>(
      find.descendant(of: actions, matching: find.byType(IconButtonTheme)),
    );
    expect(iconTheme.data.style?.iconSize?.resolve({}), 20);
    expect(
      tester.widget<Icon>(find.byIcon(Icons.delete_outline)).color,
      const Color(0xFFFF3B30),
    );
    await tester.tap(find.byTooltip('向右移动'));
    await tester.pumpAndSettle();
    expect(store.devices.map((d) => d.deviceId), ['b', 'a']);

    await tester.longPress(find.byKey(const ValueKey('device-name-a')));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('删除设备'));
    await tester.pumpAndSettle();
    expect(find.text('删除 Mac?'), findsOneWidget);
    await tester.tap(find.widgetWithText(TextButton, '删除'));
    await tester.pumpAndSettle();
    expect(store.devices.map((d) => d.deviceId), ['b']);
    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump(const Duration(seconds: 2));
  });

  testWidgets('device strip can move into the top bar', (tester) async {
    SharedPreferences.setMockInitialValues({});
    tester.view.physicalSize = const Size(600, 800);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    final store = PadStore()
      ..deviceStripPlacement = 'top'
      ..devices = [
        Device(deviceId: 'a', name: 'Mac', ips: const [], port: 9618),
        Device(deviceId: 'b', name: 'PC', ips: const [], port: 9618),
      ];
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();

    expect(find.text('未连接'), findsNothing);
    expect(find.text('连接中'), findsNothing);
    expect(find.text('已连接 0 台'), findsNothing);
    expect(find.text('0'), findsOneWidget);
    final strip = tester.widget<ListView>(
      find.byKey(const ValueKey('device-strip')),
    );
    expect(strip.clipBehavior, Clip.none);
    expect(strip.padding, const EdgeInsets.symmetric(horizontal: 10));
    final horizontalClip = tester.widget<ClipRect>(
      find.byKey(const ValueKey('device-strip-horizontal-clip')),
    );
    expect(
      horizontalClip.clipper?.getClip(const Size(100, 48)),
      const Rect.fromLTRB(1, -32, 99, 80),
    );
    expect(
      find.byKey(const ValueKey('device-strip-edge-fade')),
      findsOneWidget,
    );
    final stripRect = tester.getRect(
      find.byKey(const ValueKey('device-strip')),
    );
    final statusRect = tester.getRect(
      find.byKey(const ValueKey('connection-status')),
    );
    final inputRect = tester.getRect(find.byKey(const ValueKey('text-input')));
    final aboutRect = tester.getRect(find.byTooltip('关于'));
    final capsuleBodyRect = tester.getRect(
      find.descendant(
        of: find.byKey(const ValueKey('device-capsule-a')),
        matching: find.byType(AnimatedContainer),
      ),
    );
    expect(stripRect.height, 48);
    expect(capsuleBodyRect.top - stripRect.top, greaterThanOrEqualTo(5));
    expect(stripRect.bottom - capsuleBodyRect.bottom, greaterThanOrEqualTo(5));
    expect(stripRect.top, lessThan(inputRect.top));
    expect(stripRect.left, greaterThan(statusRect.right));
    expect(stripRect.right, lessThanOrEqualTo(aboutRect.left));
    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump(const Duration(seconds: 2));
  });

  testWidgets('dark nub uses one wheel-colored bordered panel', (tester) async {
    SharedPreferences.setMockInitialValues({'theme': 'dark'});
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.tap(find.text('轨迹球'));
    await tester.pump();

    final panel = tester.widget<DecoratedBox>(
      find.byKey(const ValueKey('nub-panel')),
    );
    final decoration = panel.decoration as BoxDecoration;
    expect(decoration.color, const Color(0xFF2E2E2E));
    expect(decoration.border?.top.color, const Color(0xFF5C5C5C));
    expect(decoration.border?.top.width, 1);
    expect(find.byKey(const ValueKey('nub-backdrop')), findsNothing);
  });

  testWidgets('device manager shows explicit reorder handles', (tester) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore()
      ..devices = [
        Device(deviceId: 'a', name: 'Mac', ips: const [], port: 9618),
      ];
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.tap(find.byKey(const ValueKey('connection-status')));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('device-drag-a')), findsOneWidget);
    expect(find.byIcon(Icons.drag_handle), findsOneWidget);
    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump(const Duration(seconds: 2));
  });

  testWidgets('edit device dialog survives its closing animation', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore()
      ..devices = [
        Device(deviceId: 'a', name: 'Mac', ips: const ['10.0.0.2'], port: 9618),
      ];
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.tap(find.byKey(const ValueKey('connection-status')));
    await tester.pumpAndSettle();
    await tester.tap(find.byIcon(Icons.edit));
    await tester.pumpAndSettle();
    await tester.tap(find.text('添加 IP'));
    await tester.pump();
    await tester.tap(find.byIcon(Icons.remove_circle_outline).last);
    await tester.pump();
    await tester.enterText(find.byKey(const ValueKey('edit-pair-code')), '12');
    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(tester.takeException(), isNull);
    expect(find.text('编辑设备'), findsNothing);
    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump(const Duration(seconds: 2));
  });

  testWidgets('about is left of settings and explains undo limits', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    expect(
      tester.getCenter(find.byTooltip('关于')).dx,
      lessThan(tester.getCenter(find.byTooltip('设置')).dx),
    );
    await tester.tap(find.byTooltip('关于'));
    await tester.pumpAndSettle();
    expect(find.text('关于 AgentsPads'), findsOneWidget);
    expect(find.byKey(const ValueKey('about-close')), findsOneWidget);
    expect(find.text('撤回的限制'), findsOneWidget);
    expect(find.textContaining('Ctrl+Z'), findsOneWidget);
    expect(find.textContaining('Cmd+Z'), findsOneWidget);
    expect(find.textContaining('终端和命令行'), findsOneWidget);
    final aboutTop = tester.getTopLeft(find.text('关于 AgentsPads')).dy;
    expect(aboutTop, greaterThanOrEqualTo(8));
    await tester.tap(find.byKey(const ValueKey('about-close')));
    await tester.pumpAndSettle();
    expect(find.text('关于 AgentsPads'), findsNothing);
  });

  testWidgets('zero-delay voice candidate sends text only once', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    store.voiceDelayMs = 0;
    store.devices = [
      Device(
        deviceId: 'pc',
        name: 'PC',
        ips: ['127.0.0.1'],
        port: 9618,
        secret: 'k',
      ),
    ];

    const ws = MethodChannel('agentpad/ws');
    const events = MethodChannel('agentpad/ws_events');
    final sendGate = Completer<bool>();
    final secondEvidence = Completer<bool>();
    var textSends = 0;
    var voiceQueries = 0;
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(ws, (
      call,
    ) async {
      fakePc(call, store.devices);
      if (call.method == 'connect') {
        return true;
      }
      if (call.method == 'voiceEvidence') {
        voiceQueries++;
        return voiceQueries == 1 ? true : secondEvidence.future;
      }
      if (call.method == 'send') {
        final raw = (call.arguments as Map)['text'] as String;
        if ((jsonDecode(raw) as Map)['type'] == 'text') {
          textSends++;
          return sendGate.future;
        }
        return true;
      }
      return null;
    });
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      events,
      (_) async => null,
    );
    addTearDown(() async {
      if (!sendGate.isCompleted) sendGate.complete(false);
      if (!secondEvidence.isCompleted) secondEvidence.complete(false);
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(ws, null);
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        events,
        null,
      );
    });

    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();
    await tester.pump();
    expect(find.text('已连接 1 台'), findsOneWidget);
    expect(
      find.byKey(const ValueKey('connection-icon-connected')),
      findsOneWidget,
    );
    await tester.tap(find.byType(TextField));
    await tester.pump();
    expect(tester.testTextInput.isVisible, isTrue);
    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: '测',
        selection: TextSelection.collapsed(offset: 1),
        composing: TextRange(start: 0, end: 1),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 1));
    expect(voiceQueries, greaterThan(0));
    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: '测试',
        selection: TextSelection.collapsed(offset: 2),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 1));
    expect(textSends, 1);
    secondEvidence.complete(true);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 1));

    expect(textSends, 1);
    sendGate.complete(false);
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('send-button')));
    await tester.pump();
    expect(textSends, 2);
  });

  testWidgets('empty input backspace is forwarded to selected computers', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    store.voiceAutoSend = false;
    store.devices = [
      Device(
        deviceId: 'pc',
        name: 'PC',
        ips: ['127.0.0.1'],
        port: 9618,
        secret: 'k',
      ),
    ];

    const ws = MethodChannel('agentpad/ws');
    const events = MethodChannel('agentpad/ws_events');
    final sent = <Map>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(ws, (
      call,
    ) async {
      fakePc(call, store.devices);
      if (call.method == 'connect') return true;
      if (call.method == 'send') {
        final raw = (call.arguments as Map)['text'] as String;
        sent.add((jsonDecode(raw) as Map).cast<String, dynamic>());
        return true;
      }
      if (call.method == 'close') return true;
      return null;
    });
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      events,
      (_) async => null,
    );
    addTearDown(() {
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(ws, null);
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        events,
        null,
      );
    });

    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();
    await tester.pump();
    sent.clear();
    await tester.tap(find.byType(TextField));
    await tester.pump();

    const codec = StandardMethodCodec();
    ServicesBinding.instance.channelBuffers.push(
      'agentpad/ws_events',
      codec.encodeSuccessEnvelope({'event': 'inputBackspace'}),
      (_) {},
    );
    await tester.pump();
    expect(sent, [
      {'type': 'key', 'key': 'Backspace', 'modifiers': <dynamic>[]},
    ]);

    // Same empty-box path: the send button is an Enter key, not a text send.
    await tester.tap(find.byKey(const ValueKey('send-button')));
    await tester.pump();
    expect(sent.last, {
      'type': 'key',
      'key': 'Enter',
      'modifiers': <dynamic>[],
    });
    sent.removeLast();

    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: 'a',
        selection: TextSelection.collapsed(offset: 1),
      ),
    );
    await tester.pump();
    ServicesBinding.instance.channelBuffers.push(
      'agentpad/ws_events',
      codec.encodeSuccessEnvelope({'event': 'inputBackspace'}),
      (_) {},
    );
    await tester.pump();
    expect(sent.length, 1);

    await tester.sendKeyEvent(LogicalKeyboardKey.backspace);
    await tester.pump();
    expect(
      tester
          .widget<TextField>(find.byKey(const ValueKey('text-input')))
          .controller!
          .text,
      isEmpty,
    );
    expect(sent.length, 1);
    await tester.sendKeyDownEvent(LogicalKeyboardKey.backspace);
    await tester.sendKeyRepeatEvent(LogicalKeyboardKey.backspace);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.backspace);
    await tester.pump();
    expect(sent.length, 3);

    // A stale IME delete right after send-and-clear must not reach the PC.
    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: 'hi',
        selection: TextSelection.collapsed(offset: 2),
      ),
    );
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('send-button')));
    await tester.pump();
    expect(sent.length, 4);
    expect(sent.last['type'], 'text');
    ServicesBinding.instance.channelBuffers.push(
      'agentpad/ws_events',
      codec.encodeSuccessEnvelope({'event': 'inputBackspace'}),
      (_) {},
    );
    await tester.pump();
    expect(sent.length, 4);
    await tester.pump(const Duration(seconds: 1));
    ServicesBinding.instance.channelBuffers.push(
      'agentpad/ws_events',
      codec.encodeSuccessEnvelope({'event': 'inputBackspace'}),
      (_) {},
    );
    await tester.pump();
    expect(sent.length, 5);
    expect(sent.last['key'], 'Backspace');

    await tester.tap(find.byTooltip('设置'));
    await tester.pumpAndSettle();
    ServicesBinding.instance.channelBuffers.push(
      'agentpad/ws_events',
      codec.encodeSuccessEnvelope({'event': 'inputBackspace'}),
      (_) {},
    );
    await tester.pump();
    expect(sent.length, 5);

    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('keyboard open does not overflow', (tester) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    tester.view.physicalSize = const Size(1080, 2400);
    tester.view.devicePixelRatio = 3;
    tester.view.viewInsets = const FakeViewPadding(bottom: 420);
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    addTearDown(tester.view.resetViewInsets);
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();
    expect(find.byKey(const ValueKey('send-button')), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('portrait display split window keeps portrait layout', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    tester.view.physicalSize = const Size(640, 360);
    tester.view.display.size = const Size(1080, 2400);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.display.resetSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();

    expect(find.byKey(const ValueKey('landscape-pointer-pane')), findsNothing);
    expect(find.byKey(const ValueKey('page-scroll')), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('force landscape waits for the real viewport to rotate', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore()..forceLandscape = true;
    tester.view.physicalSize = const Size(390, 844);
    tester.view.display.size = const Size(1080, 2400);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.display.resetSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();

    expect(find.byKey(const ValueKey('landscape-pointer-pane')), findsNothing);
    expect(find.byKey(const ValueKey('landscape-input-pane')), findsNothing);
    expect(find.byKey(const ValueKey('page-scroll')), findsOneWidget);
    expect(tester.takeException(), isNull);

    tester.view.physicalSize = const Size(844, 390);
    tester.view.display.size = const Size(2400, 1080);
    await tester.pump();
    expect(find.byKey(const ValueKey('landscape-input-pane')), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets(
    'small landscape windows and large text fall back to one column',
    (tester) async {
      SharedPreferences.setMockInitialValues({});
      tester.view.devicePixelRatio = 1;
      tester.view.display.size = const Size(2400, 1080);
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      addTearDown(tester.view.display.resetSize);
      addTearDown(tester.view.resetViewInsets);
      addTearDown(tester.platformDispatcher.clearTextScaleFactorTestValue);
      for (final scenario in [
        (const Size(500, 360), 0.0, 1.0, 'medium'),
        (const Size(640, 360), 200.0, 1.0, 'medium'),
        (const Size(844, 390), 0.0, 1.6, 'medium'),
        (const Size(640, 360), 0.0, 1.0, 'huge'),
      ]) {
        tester.view.physicalSize = scenario.$1;
        tester.view.viewInsets = FakeViewPadding(bottom: scenario.$2);
        tester.platformDispatcher.textScaleFactorTestValue = scenario.$3;
        final store = PadStore()..inputHeight = scenario.$4;
        await tester.pumpWidget(
          AgentsPadsApp(
            key: UniqueKey(),
            store: store,
            enableAutomaticUpdateChecks: false,
          ),
        );
        await tester.pump();
        expect(
          find.byKey(const ValueKey('landscape-input-pane')),
          findsNothing,
        );
        expect(tester.takeException(), isNull);
      }
    },
  );

  testWidgets(
    'landscape splits into scrollable pointer and fixed input panes',
    (tester) async {
      SharedPreferences.setMockInitialValues({});
      final store = PadStore();
      await store.load();
      for (var i = 0; i < 20; i++) {
        store.shortcuts.add(
          Shortcut(id: 'extra-$i', label: 'F$i', key: 'F$i', modifiers: []),
        );
      }
      tester.view.physicalSize = const Size(640, 360);
      tester.view.display.size = const Size(2400, 1080);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.display.resetSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      await tester.pumpWidget(
        AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
      );
      await tester.pump();

      final pointerPane = find.byKey(const ValueKey('landscape-pointer-pane'));
      final inputPane = find.byKey(const ValueKey('landscape-input-pane'));
      expect(pointerPane, findsOneWidget);
      expect(inputPane, findsOneWidget);
      expect(
        tester.getCenter(pointerPane).dx,
        greaterThan(tester.getCenter(inputPane).dx),
      );
      expect(
        tester.getSize(pointerPane).width,
        tester.getSize(inputPane).width,
      );
      expect(
        find.descendant(
          of: pointerPane,
          matching: find.byKey(const ValueKey('landscape-pointer-scroll')),
        ),
        findsOneWidget,
      );
      expect(
        find.ancestor(
          of: inputPane,
          matching: find.byType(SingleChildScrollView),
        ),
        findsNothing,
      );
      expect(
        find.descendant(
          of: pointerPane,
          matching: find.byKey(const ValueKey('pointer-area')),
        ),
        findsOneWidget,
      );
      expect(
        tester.getSize(find.byKey(const ValueKey('pointer-area'))).aspectRatio,
        greaterThan(2),
      );
      expect(
        find.descendant(of: inputPane, matching: find.byType(TextField)),
        findsOneWidget,
      );
      final inputTop = tester.getTopLeft(inputPane).dy;
      final pointerTop = tester
          .getTopLeft(find.byKey(const ValueKey('pointer-area')))
          .dy;
      await tester.dragFrom(
        tester.getCenter(pointerPane),
        const Offset(0, -120),
      );
      await tester.pumpAndSettle();
      expect(
        tester.getTopLeft(find.byKey(const ValueKey('pointer-area'))).dy,
        lessThan(pointerTop),
      );
      expect(tester.getTopLeft(inputPane).dy, inputTop);
      expect(tester.takeException(), isNull);
    },
  );

  testWidgets('landscape pointer side setting swaps both panes', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    tester.view.physicalSize = const Size(640, 360);
    tester.view.display.size = const Size(2400, 1080);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.display.resetSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();

    await tester.tap(find.byTooltip('设置'));
    await tester.pumpAndSettle();
    final setting = find.byKey(
      const ValueKey('landscape-pointer-side-setting'),
    );
    expect(setting, findsOneWidget);
    await tester.ensureVisible(setting);
    await tester.pumpAndSettle();
    expect(store.landscapePointerSide, 'right');
    await tester.tap(find.descendant(of: setting, matching: find.text('触控在左')));
    await tester.pumpAndSettle();
    expect(store.landscapePointerSide, 'left');
    tester.state<NavigatorState>(find.byType(Navigator)).pop();
    await tester.pumpAndSettle();

    final pointerPane = find.byKey(const ValueKey('landscape-pointer-pane'));
    final inputPane = find.byKey(const ValueKey('landscape-input-pane'));
    expect(
      tester.getCenter(pointerPane).dx,
      lessThan(tester.getCenter(inputPane).dx),
    );
    expect(tester.getSize(pointerPane).width, tester.getSize(inputPane).width);
    expect(tester.takeException(), isNull);
  });

  testWidgets('home and settings open the same available update dialog', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    await store.load();
    await tester.pumpWidget(
      AgentsPadsApp(store: store, enableAutomaticUpdateChecks: false),
    );
    await tester.pump();

    final dynamic home = tester.state(find.byType(HomePage));
    home.setState(() {
      home.checkingUpdate = false;
      home.pendingUpdateTag = null;
      home.pendingUpdateBody = null;
      home.pendingUpdateApkUrl = null;
    });
    await tester.pump();
    final idleFooter = tester.widget<InkWell>(
      find.byKey(const ValueKey('update-footer')),
    );
    expect(idleFooter.onTap, isNotNull);

    home.setState(() {
      home.pendingUpdateTag = '9.9.9';
      home.pendingUpdateBody = 'release notes';
      home.pendingUpdateApkUrl = 'https://example.com/agentspads.apk';
    });
    await tester.pump();

    await tester.tap(find.byKey(const ValueKey('update-footer')));
    await tester.pumpAndSettle();
    expect(find.text('发现新版本 v9.9.9'), findsOneWidget);
    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();

    await tester.tap(find.byTooltip('设置'));
    await tester.pumpAndSettle();
    final action = find.byKey(const ValueKey('settings-update-action'));
    expect(action, findsOneWidget);
    expect(
      find.descendant(of: action, matching: find.text('可更新')),
      findsOneWidget,
    );
    await tester.ensureVisible(action);
    await tester.tap(action);
    await tester.pumpAndSettle();
    expect(find.text('发现新版本 v9.9.9'), findsOneWidget);
  });
}
