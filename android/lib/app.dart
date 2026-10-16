import 'dart:async';
import 'dart:convert' show jsonDecode, utf8;
import 'dart:io' show HttpClient;
import 'dart:math' as math;
import 'dart:ui' show ImageFilter, TileMode;
import 'package:flutter/services.dart'
    show
        DeviceOrientation,
        HapticFeedback,
        KeyDownEvent,
        KeyRepeatEvent,
        LogicalKeyboardKey,
        MethodChannel,
        PlatformException,
        SystemChrome;

import 'package:flutter/foundation.dart' show kDebugMode;
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'hub.dart';
import 'pointer.dart';
import 'diagnostic_log.dart';
import 'protocol.dart';
import 'store.dart';
import 'touchpad.dart';
import 'voice_input.dart';

const appVersion = kDebugMode
    ? 'debug'
    : String.fromEnvironment('AGENTPAD_VERSION', defaultValue: '0.1.0');

class AndroidUpdateInfo {
  const AndroidUpdateInfo({
    required this.tagName,
    required this.version,
    required this.body,
    required this.apkUrl,
    required this.sha256,
  });

  final String tagName;
  final String version;
  final String body;
  final String apkUrl;
  final String sha256;
}

List<Uri> androidUpdateManifestUris([DateTime? now]) {
  final hour =
      (now ?? DateTime.now()).toUtc().millisecondsSinceEpoch ~/ 3600000;
  const path =
      'gh/MitsukiJoe/AgentsPads@update-manifest/agentspads-update.json';
  return [
    Uri.parse(
      'https://github.com/MitsukiJoe/AgentsPads/releases/latest/download/agentspads-update.json',
    ),
    Uri.parse('https://cdn.jsdelivr.net/$path?hour=$hour'),
    Uri.parse('https://cdn.jsdmirror.com/$path?hour=$hour'),
  ];
}

/// 只接受 `X.Y.Z` 纯 ASCII 数字；版本会拼进下载 URL，不能带任何路径字符。
List<int>? parseAppVersion(String value) {
  if (!RegExp(r'^[0-9]+\.[0-9]+\.[0-9]+$').hasMatch(value)) return null;
  final parts = value.split('.').map(int.tryParse).toList();
  return parts.contains(null) ? null : parts.cast<int>();
}

bool isNewerAppVersion(String remote, String current) {
  final remoteParts = parseAppVersion(remote);
  final currentParts = parseAppVersion(current);
  if (remoteParts == null || currentParts == null) return false;
  for (var i = 0; i < 3; i++) {
    if (remoteParts[i] != currentParts[i]) {
      return remoteParts[i] > currentParts[i];
    }
  }
  return false;
}

/// 「跟随系统」按系统亮度解析成 white / black；已指定的浅色或深色原样返回。
String resolveAppIconTarget(String pref, Brightness platformBrightness) {
  if (pref == 'black' || pref == 'white') return pref;
  return platformBrightness == Brightness.dark ? 'black' : 'white';
}

/// 原生待处理状态只有完整操作成功后才清除，部分失败时仍允许重试。
bool appIconRestartHintVisible(String? pending, String current) =>
    pending == 'white' || pending == 'black';

AndroidUpdateInfo newerAndroidUpdate(
  AndroidUpdateInfo? current,
  AndroidUpdateInfo candidate,
) {
  if (current == null ||
      isNewerAppVersion(candidate.version, current.version)) {
    return candidate;
  }
  return current;
}

AndroidUpdateInfo? parseAndroidUpdateManifest(String raw) {
  try {
    final data = jsonDecode(raw);
    if (data is! Map || data['schema'] != 1 || data['assets'] is! Map) {
      return null;
    }
    final tagName = data['tag_name'] as String? ?? '';
    final version = data['version'] as String? ?? '';
    final releaseUrl = data['release_url'] as String? ?? '';
    final body = data['body'] as String? ?? '';
    final assets = data['assets'] as Map;
    final android = assets['android'];
    if (android is! Map ||
        tagName != 'v$version' ||
        parseAppVersion(version) == null) {
      return null;
    }
    final apkUrl = android['url'] as String? ?? '';
    final sha256 = (android['sha256'] as String? ?? '').toLowerCase();
    final expectedUrl =
        'https://github.com/MitsukiJoe/AgentsPads/releases/download/$tagName/agentspads.apk';
    if (apkUrl != expectedUrl || !RegExp(r'^[0-9a-f]{64}$').hasMatch(sha256)) {
      return null;
    }
    return AndroidUpdateInfo(
      tagName: tagName,
      version: version,
      body: body.trim().isEmpty ? '新版本已发布：$releaseUrl' : body,
      apkUrl: apkUrl,
      sha256: sha256,
    );
  } catch (_) {
    return null;
  }
}

class AgentsPadsApp extends StatefulWidget {
  const AgentsPadsApp({
    super.key,
    this.store,
    this.enableAutomaticUpdateChecks = true,
  });

  final PadStore? store;
  final bool enableAutomaticUpdateChecks;

  @override
  State<AgentsPadsApp> createState() => _AgentsPadsAppState();
}

class _AgentsPadsAppState extends State<AgentsPadsApp> {
  late final PadStore store;
  var ready = false;

  @override
  void initState() {
    super.initState();
    store = widget.store ?? PadStore();
    if (widget.store != null) {
      ready = true;
      unawaited(_applyOrientation());
    } else {
      store.load().then((_) async {
        await _applyOrientation();
        if (mounted) setState(() => ready = true);
      });
    }
  }

  Future<void> _applyOrientation() {
    return SystemChrome.setPreferredOrientations(
      store.forceLandscape
          ? const [
              DeviceOrientation.landscapeLeft,
              DeviceOrientation.landscapeRight,
            ]
          : const [],
    );
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'AgentsPads',
      theme: _appTheme(Brightness.light),
      darkTheme: _appTheme(Brightness.dark),
      themeMode: switch (store.theme) {
        'light' => ThemeMode.light,
        'dark' => ThemeMode.dark,
        _ => ThemeMode.system,
      },
      home: ready
          ? HomePage(
              store: store,
              enableAutomaticUpdateChecks: widget.enableAutomaticUpdateChecks,
              onThemeChanged: () => setState(() {}),
              onLayoutChanged: () {
                unawaited(_applyOrientation());
                setState(() {});
              },
            )
          : const Scaffold(body: SizedBox.shrink()),
    );
  }
}

class HomePage extends StatefulWidget {
  const HomePage({
    super.key,
    required this.store,
    required this.enableAutomaticUpdateChecks,
    this.onThemeChanged,
    this.onLayoutChanged,
  });
  final PadStore store;
  final bool enableAutomaticUpdateChecks;
  final VoidCallback? onThemeChanged;
  final VoidCallback? onLayoutChanged;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> with WidgetsBindingObserver {
  static const _pageGutter = 12.0;
  static const _deviceStripFadeWidth = 10.0;
  static const _updateCheckInterval = Duration(hours: 24);
  static const _backspaceMuteAfterSend = Duration(seconds: 1);

  late final PadStore store;
  Hub? hub;
  VoidCallback? _refreshConnectedSheet;
  VoidCallback? _refreshSettingsSheet;
  final input = TextEditingController();
  final inputFocus = FocusNode();
  final pointer = PointerCoalescer();
  final pointerCadence = PointerCadence();
  final pointerClock = Stopwatch()..start();
  final touchpad = TouchpadGesture();
  final voiceInput = VoiceInputDetector();
  Timer? pointerTimer;
  Timer? trackPointTimer;
  Timer? padLongPressTimer;
  Timer? voiceTimer;
  Timer? backspaceMuteTimer;
  Timer? updateNoticeTimer;
  Timer? updateCheckTimer;
  OverlayEntry? updateNoticeEntry;
  var checkingUpdate = false;
  String? pendingUpdateTag;
  String? pendingUpdateBody;
  String? pendingUpdateApkUrl;
  String _appIconCurrent = 'white';
  String? _appIconPending;
  Offset? trackPointVec;
  var leftDown = false;
  var rightDown = false;
  var pointerActive = false;
  final activePointerIds = <int>{};
  String? lastSent;
  var sendingText = false;

  int get _btns => (leftDown ? 1 : 0) | (rightDown ? 2 : 0);

  double get _trackpadHeight => switch (store.pointerSize) {
    'small' => 104,
    'large' => 240,
    _ => 136,
  };

  double get _inputBoxHeight => switch (store.inputHeight) {
    'tall' => 196,
    'huge' => 280,
    _ => 112,
  };

  @override
  void initState() {
    super.initState();
    unawaited(DiagnosticLog.refresh());
    store = widget.store;
    WidgetsBinding.instance.addObserver(this);
    hub = Hub(
      store,
      active: _connectionsActive,
      onTargetsChanged: _cancelTargetGesture,
      onChange: () {
        if (mounted) setState(() {});
        _refreshConnectedSheet?.call();
      },
    );
    input.addListener(_onInputChanged);
    inputFocus.addListener(_onInputFocusChanged);
    inputFocus.onKeyEvent = (_, event) {
      if ((event is KeyDownEvent || event is KeyRepeatEvent) &&
          event.logicalKey == LogicalKeyboardKey.backspace &&
          input.text.isEmpty) {
        _handleInputBackspace();
        return KeyEventResult.handled;
      }
      return KeyEventResult.ignored;
    };
    NativeWs.inputBackspaceHandler = _handleInputBackspace;
    hub!.sync();
    unawaited(_applyPointerHzDefault());
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      unawaited(_applyAppIcon(store.appIcon));
      if (widget.enableAutomaticUpdateChecks) {
        unawaited(_checkAndroidUpdate(notify: false));
      }
    });
    if (widget.enableAutomaticUpdateChecks) {
      updateCheckTimer = Timer.periodic(_updateCheckInterval, (_) {
        if (mounted) unawaited(_checkAndroidUpdate(notify: false));
      });
    }
  }

  @override
  void didChangePlatformBrightness() {
    if (store.appIcon != 'system') return;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) unawaited(_applyAppIcon(store.appIcon));
    });
  }

  bool get _connectionsActive =>
      switch (WidgetsBinding.instance.lifecycleState) {
        AppLifecycleState.hidden ||
        AppLifecycleState.paused ||
        AppLifecycleState.detached => false,
        _ => true,
      };

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    hub?.setActive(_connectionsActive);
    if (_connectionsActive && state == AppLifecycleState.resumed) {
      hub?.nudge();
    }
    if (!_connectionsActive) {
      _stopTrackPoint();
      pointerTimer?.cancel();
      pointerTimer = null;
      padLongPressTimer?.cancel();
      pointer.tick();
      pointerCadence.reset();
      touchpad.cancel();
      activePointerIds.clear();
      _ignoredPointerIds.clear();
      pointerActive = leftDown = rightDown = false;
    }
  }

  Future<void> _applyPointerHzDefault() async {
    // One-shot: older builds read current 60Hz mode and/or left a manual pick.
    // Re-default from peak supported rate once, then honor later manual choices.
    final prefs = await SharedPreferences.getInstance();
    if (!(prefs.getBool('pointer_hz_peak_v1') ?? false)) {
      store.pointerHzManual = false;
      await prefs.setBool('pointer_hz_peak_v1', true);
    }
    if (store.pointerHzManual) return;
    final measured = await NativeWs.displayRefreshHz();
    final next = pointerHzForDisplay(measured);
    if (store.pointerHz == next) return;
    store.pointerHz = next;
    store.pointerHzManual = false;
    _retunePointerCadence();
    await store.save();
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    pointerTimer?.cancel();
    trackPointTimer?.cancel();
    padLongPressTimer?.cancel();
    voiceTimer?.cancel();
    backspaceMuteTimer?.cancel();
    updateNoticeTimer?.cancel();
    updateCheckTimer?.cancel();
    updateNoticeEntry?.remove();
    NativeWs.inputBackspaceHandler = null;
    hub?.dispose();
    input.removeListener(_onInputChanged);
    inputFocus.removeListener(_onInputFocusChanged);
    inputFocus.dispose();
    input.dispose();
    super.dispose();
  }

  Future<void> _persist({bool theme = false, bool layout = false}) async {
    await store.save();
    hub?.sync();
    if (theme) widget.onThemeChanged?.call();
    if (layout) widget.onLayoutChanged?.call();
    setState(() {});
  }

  Future<void> _sendText(String mode) async {
    voiceTimer?.cancel();
    final h = hub;
    if (h == null) return;
    final text = input.text;
    if (text.isEmpty || sendingText) return;
    sendingText = true;
    try {
      final json = textMsg(text, autoEnter: store.autoEnter, sendMode: mode);
      if (await h.sendSelected(json)) {
        lastSent = text;
        await NativeWs.resetVoiceEvidence();
        voiceInput.reset();
        input.clear();
        // Voice IMEs may still issue a stale correction delete against the
        // text we just cleared; don't let that reach the PC as a Backspace.
        backspaceMuteTimer?.cancel();
        backspaceMuteTimer = Timer(_backspaceMuteAfterSend, () {});
        setState(() {});
      }
    } finally {
      sendingText = false;
    }
  }

  void _onInputChanged() {
    voiceTimer?.cancel();
    final value = input.value;
    if (!store.voiceAutoSend || !inputFocus.hasFocus) {
      voiceInput.reset();
      return;
    }
    _updateVoiceCandidate(value);
    NativeWs.voiceEvidence().then((hasEvidence) {
      if (!mounted ||
          !store.voiceAutoSend ||
          !inputFocus.hasFocus ||
          input.value != value ||
          !hasEvidence) {
        return;
      }
      _updateVoiceCandidate(value, voiceEvidence: true);
    });
  }

  void _onInputFocusChanged() {
    voiceTimer?.cancel();
    voiceInput.reset();
    NativeWs.resetVoiceEvidence();
  }

  void _handleInputBackspace() {
    if (!inputFocus.hasFocus || input.text.isNotEmpty) return;
    if (backspaceMuteTimer?.isActive ?? false) return;
    final h = hub;
    if (h == null) return;
    unawaited(h.sendSelected(keyMsg('Backspace', const [])));
  }

  void _sendEmptyEnter() {
    if (input.text.isNotEmpty) return;
    voiceTimer?.cancel();
    unawaited(hub?.sendSelected(keyMsg('Enter', const [])));
  }

  void _updateVoiceCandidate(
    TextEditingValue value, {
    bool voiceEvidence = false,
  }) {
    final ready = voiceInput.update(value, voiceEvidence: voiceEvidence);
    if (!ready) return;
    voiceTimer?.cancel();
    voiceTimer = Timer(store.voiceDelay, () {
      if (mounted &&
          store.voiceAutoSend &&
          inputFocus.hasFocus &&
          input.value == value) {
        _sendText('commit');
      }
    });
  }

  Future<void> _undo() async {
    await hub?.sendSelected(undoMsg());
    final prev = lastSent;
    if (prev == null) return;
    lastSent = null;
    input.value = TextEditingValue(
      text: prev,
      selection: TextSelection.collapsed(offset: prev.length),
    );
    voiceTimer?.cancel();
    voiceInput.reset();
    setState(() {});
  }

  Duration get _pointerNow => pointerClock.elapsed;

  Duration get _pointerPeriod =>
      Duration(microseconds: (1000000 / store.pointerHz).round());

  void _retunePointerCadence() {
    pointerCadence.period = _pointerPeriod;
    pointerCadence.reset();
    pointerTimer?.cancel();
    pointerTimer = null;
    if (trackPointVec != null) {
      trackPointTimer?.cancel();
      trackPointTimer = Timer.periodic(
        _pointerPeriod,
        (_) => _emitTrackPoint(),
      );
    }
  }

  void _emitPointer(Map<String, num> p, {bool immediate = false}) {
    final dx = (p['dx'] as num).toDouble();
    final dy = (p['dy'] as num).toDouble();
    final buttons = (p['buttons'] as num).toInt();
    final wheel = (p['wheel'] as num).toDouble();
    final h = hub;
    if (h == null) return;
    h.sendPointer(dx, dy, buttons, wheel, immediate: immediate);
  }

  void _flushPointer() {
    final packet = pointer.tick();
    if (packet == null) return;
    _emitPointer(packet, immediate: true);
  }

  void _addPointer(double dx, double dy, int buttons, double wheel) {
    if (_ignoredPointerIds.isNotEmpty) return;
    pointer.add(dx, dy, buttons, wheel);
    if (pointerCadence.due(_pointerNow)) _flushPointer();
    _startPointerTimer();
  }

  void _startPointerTimer() {
    pointerCadence.period = _pointerPeriod;
    if (pointerTimer?.isActive == true) return;
    pointerTimer = Timer.periodic(pointerCadence.period, (timer) {
      if (pointer.pending && pointerCadence.due(_pointerNow)) _flushPointer();
      if (!pointerActive && !pointer.pending) {
        timer.cancel();
        if (identical(pointerTimer, timer)) pointerTimer = null;
        pointerCadence.reset();
      }
    });
  }

  void _queueImmediatePointer(double dx, double dy, int buttons, double wheel) {
    if (_ignoredPointerIds.isNotEmpty) return;
    final pending = pointer.tick();
    if (pending != null) _emitPointer(pending);
    pointer.add(dx, dy, buttons, wheel);
    _emitPointer(pointer.tick()!, immediate: true);
    pointerCadence.markSent(_pointerNow);
    _startPointerTimer();
  }

  int get _onlineCount {
    final h = hub;
    if (h == null) return 0;
    return store.devices.where((d) => h.isOnline(d)).length;
  }

  @override
  Widget build(BuildContext context) {
    return PopScope(
      canPop: false,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop && ModalRoute.of(context)?.isCurrent == true) {
          unawaited(_finishForAppIcon());
        }
      },
      child: Scaffold(
        resizeToAvoidBottomInset: true,
        body: SafeArea(
          child: LayoutBuilder(
            builder: (context, constraints) {
              final display = View.of(context).display.size;
              final textScale = math.max(
                1.0,
                MediaQuery.textScalerOf(context).scale(14) / 14,
              );
              return constraints.maxWidth > constraints.maxHeight &&
                      display.width > display.height &&
                      constraints.maxWidth >= 640 * textScale &&
                      constraints.maxHeight >= _inputBoxHeight + 160 * textScale
                  ? _landscapeLayout()
                  : _portraitLayout(constraints);
            },
          ),
        ),
      ),
    );
  }

  Widget _portraitLayout(BoxConstraints constraints) => SingleChildScrollView(
    key: const ValueKey('page-scroll'),
    physics: pointerActive ? const NeverScrollableScrollPhysics() : null,
    keyboardDismissBehavior: ScrollViewKeyboardDismissBehavior.manual,
    child: ConstrainedBox(
      constraints: BoxConstraints(minHeight: constraints.maxHeight),
      child: Column(
        children: [
          _topBar(),
          _actionButtons(),
          _textInput(),
          if (store.devices.isNotEmpty && store.deviceStripPlacement == 'input')
            _deviceStrip(),
          if (store.homePointerQuickSwitch) _pointerModeSelector(),
          _pointerArea(),
          _shortcutSection(),
          _updateFooter(),
        ],
      ),
    ),
  );

  Widget _landscapeLayout() {
    final pointerPane = Expanded(child: _landscapePointerPane());
    final inputPane = Expanded(child: _landscapeInputPane());
    const divider = VerticalDivider(width: 1, thickness: 1);
    return Row(
      children: store.landscapePointerSide == 'left'
          ? [pointerPane, divider, inputPane]
          : [inputPane, divider, pointerPane],
    );
  }

  Widget _landscapePointerPane() => SizedBox.expand(
    key: const ValueKey('landscape-pointer-pane'),
    child: SingleChildScrollView(
      key: const ValueKey('page-scroll'),
      physics: pointerActive ? const NeverScrollableScrollPhysics() : null,
      keyboardDismissBehavior: ScrollViewKeyboardDismissBehavior.manual,
      child: KeyedSubtree(
        key: const ValueKey('landscape-pointer-scroll'),
        child: Column(
          children: [
            if (store.devices.isNotEmpty &&
                store.deviceStripPlacement == 'input')
              _deviceStrip(),
            if (store.homePointerQuickSwitch) _pointerModeSelector(),
            _pointerArea(),
            _shortcutSection(),
          ],
        ),
      ),
    ),
  );

  Widget _landscapeInputPane() => SizedBox.expand(
    key: const ValueKey('landscape-input-pane'),
    child: Column(
      children: [
        _topBar(),
        _actionButtons(),
        _textInput(),
        const Spacer(),
        _updateFooter(),
      ],
    ),
  );

  Widget _topBar() => Padding(
    padding: const EdgeInsets.fromLTRB(_pageGutter, 4, _pageGutter, 0),
    child: Row(
      children: [
        TextButton(
          key: const ValueKey('connection-status'),
          style: TextButton.styleFrom(
            foregroundColor: _connectionColor,
            minimumSize: const Size(0, 40),
            padding: const EdgeInsets.symmetric(horizontal: 12),
            textStyle: const TextStyle(fontWeight: FontWeight.w700),
            shape: RoundedRectangleBorder(
              borderRadius: BorderRadius.circular(14),
            ),
          ),
          onPressed: _openConnected,
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              _connectionIcon(),
              const SizedBox(width: 8),
              Text(_connectionText),
            ],
          ),
        ),
        if (store.devices.isNotEmpty && store.deviceStripPlacement == 'top')
          Expanded(
            child: Padding(
              padding: const EdgeInsets.only(left: 6),
              child: _deviceStrip(embedded: true),
            ),
          )
        else
          const Spacer(),
        IconButton(
          tooltip: '关于',
          onPressed: _openAbout,
          icon: const Icon(Icons.info_outline),
        ),
        IconButton(
          tooltip: '设置',
          onPressed: _openSettings,
          icon: const Icon(Icons.settings),
        ),
      ],
    ),
  );

  Widget _actionButtons() => Padding(
    padding: const EdgeInsets.fromLTRB(_pageGutter, 8, _pageGutter, 0),
    child: Row(
      children: [
        Expanded(
          child: _actionCapsule(
            key: const ValueKey('action-voice-auto-send'),
            selected: store.voiceAutoSend,
            label: '语音自动发送',
            onPressed: () {
              store.voiceAutoSend = !store.voiceAutoSend;
              voiceTimer?.cancel();
              voiceInput.reset();
              NativeWs.resetVoiceEvidence();
              _persist();
            },
          ),
        ),
        const SizedBox(width: 6),
        Expanded(
          child: _actionCapsule(
            key: const ValueKey('action-auto-enter'),
            selected: store.autoEnter,
            label: '电脑自动回车',
            onPressed: () {
              store.autoEnter = !store.autoEnter;
              _persist();
            },
          ),
        ),
        const SizedBox(width: 6),
        Expanded(
          child: _actionCapsule(
            key: const ValueKey('action-undo'),
            selected: true,
            label: '撤回上次输入',
            accentBorder: false,
            glow: false,
            onPressed: _undo,
          ),
        ),
      ],
    ),
  );

  Widget _textInput() {
    final accent = _accentColor;
    final border = OutlineInputBorder(borderSide: BorderSide(color: accent));
    final focused = OutlineInputBorder(
      borderSide: BorderSide(color: accent, width: 2),
    );
    const sideW = 44.0;
    const sideGap = 8.0;
    final sideRadius = BorderRadius.circular(8);
    return Padding(
      padding: const EdgeInsets.fromLTRB(_pageGutter, 12, _pageGutter, 8),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Expanded(
            child: SizedBox(
              height: _inputBoxHeight,
              child: TextField(
                key: const ValueKey('text-input'),
                controller: input,
                focusNode: inputFocus,
                maxLines: null,
                expands: true,
                textAlignVertical: TextAlignVertical.top,
                textInputAction: TextInputAction.newline,
                decoration: InputDecoration(
                  border: border,
                  enabledBorder: border,
                  focusedBorder: focused,
                  hintText: '打字或用语音键盘',
                  contentPadding: const EdgeInsets.symmetric(
                    horizontal: 12,
                    vertical: 10,
                  ),
                ),
              ),
            ),
          ),
          const SizedBox(width: 8),
          SizedBox(
            width: sideW,
            height: _inputBoxHeight,
            child: Column(
              children: [
                SizedBox(
                  width: sideW,
                  height: sideW,
                  child: Tooltip(
                    message: switch (store.inputHeight) {
                      'tall' => '输入框高度：中',
                      'huge' => '输入框高度：高',
                      _ => '输入框高度：矮',
                    },
                    child: _PressScale(
                      enabled: !store.reduceMotion,
                      child: OutlinedButton(
                        key: const ValueKey('input-height'),
                        style: OutlinedButton.styleFrom(
                          padding: EdgeInsets.zero,
                          side: BorderSide(color: accent),
                          foregroundColor: _controlForeground(
                            Theme.of(context).brightness,
                          ),
                          shape: RoundedRectangleBorder(
                            borderRadius: sideRadius,
                          ),
                        ),
                        onPressed: _cycleInputHeight,
                        child: const Icon(Icons.open_in_full, size: 18),
                      ),
                    ),
                  ),
                ),
                const SizedBox(height: sideGap),
                Expanded(
                  // Same empty-box test as _handleInputBackspace: no draft
                  // text means the button is a plain Enter key on the PC.
                  child: ValueListenableBuilder<TextEditingValue>(
                    valueListenable: input,
                    builder: (context, value, _) {
                      final empty = value.text.isEmpty;
                      return Tooltip(
                        message: empty ? '回车' : '发送',
                        child: FilledButton(
                          key: const ValueKey('send-button'),
                          style: FilledButton.styleFrom(
                            padding: EdgeInsets.zero,
                            backgroundColor: accent,
                            foregroundColor: _onAccentColor,
                            shape: RoundedRectangleBorder(
                              borderRadius: sideRadius,
                            ),
                          ),
                          onPressed: empty
                              ? _sendEmptyEnter
                              : () => _sendText('submit'),
                          child: AnimatedSwitcher(
                            duration: store.reduceMotion
                                ? Duration.zero
                                : const Duration(milliseconds: 180),
                            switchInCurve: Curves.easeOutBack,
                            switchOutCurve: Curves.easeIn,
                            transitionBuilder: (child, anim) => ScaleTransition(
                              scale: anim,
                              child: FadeTransition(
                                opacity: anim,
                                child: child,
                              ),
                            ),
                            child: Icon(
                              empty
                                  ? Icons.keyboard_return
                                  : Icons.send_rounded,
                              key: ValueKey(empty),
                              size: 20,
                            ),
                          ),
                        ),
                      );
                    },
                  ),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }

  void _cycleInputHeight() {
    setState(() {
      store.inputHeight = switch (store.inputHeight) {
        'medium' => 'tall',
        'tall' => 'huge',
        _ => 'medium',
      };
    });
    _persist();
  }

  Widget _pointerModeSelector() => Padding(
    padding: const EdgeInsets.symmetric(horizontal: _pageGutter),
    child: SizedBox(
      key: const ValueKey('home-pointer-mode'),
      width: double.infinity,
      child: SegmentedButton<String>(
        style: _segmentStyle(),
        showSelectedIcon: false,
        segments: const [
          ButtonSegment(value: 'trackpad', label: Text('触控板')),
          ButtonSegment(value: 'trackball', label: Text('轨迹球')),
          ButtonSegment(value: 'trackpoint', label: Text('小红点')),
        ],
        selected: {store.pointerMode},
        onSelectionChanged: (s) {
          _stopTrackPoint();
          store.pointerMode = s.first;
          _persist();
        },
      ),
    ),
  );

  Widget _pointerArea() {
    final trackpad = store.pointerMode == 'trackpad';
    final body = Listener(
      key: const ValueKey('pointer-input'),
      behavior: HitTestBehavior.opaque,
      onPointerDown: (e) => _setPointerActive(e, true),
      onPointerUp: (e) => _setPointerActive(e, false),
      onPointerCancel: (e) => _setPointerActive(e, false),
      child: ExcludeFocus(
        child: switch (store.pointerMode) {
          'trackball' => _nub(trackball: true),
          'trackpoint' => _nub(trackball: false),
          _ => _pad(),
        },
      ),
    );
    final padded = Padding(
      padding: const EdgeInsets.fromLTRB(_pageGutter, 8, _pageGutter, 8),
      child: body,
    );
    if (!trackpad) {
      return KeyedSubtree(key: const ValueKey('pointer-area'), child: padded);
    }
    return SizedBox(
      key: const ValueKey('pointer-area'),
      height: _trackpadHeight,
      child: padded,
    );
  }

  Widget _shortcutSection() => Padding(
    key: const ValueKey('shortcut-section'),
    padding: const EdgeInsets.fromLTRB(_pageGutter, 4, _pageGutter, 12),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        InkWell(
          key: const ValueKey('shortcut-title'),
          borderRadius: BorderRadius.circular(8),
          onTap: _showShortcutPreview,
          child: const Padding(
            padding: EdgeInsets.symmetric(vertical: 4),
            child: Row(
              children: [
                Icon(Icons.keyboard_alt_outlined, size: 18),
                SizedBox(width: 6),
                Text('快捷键'),
                SizedBox(width: 4),
                Text('测试', style: TextStyle(fontSize: 11)),
              ],
            ),
          ),
        ),
        const SizedBox(height: 4),
        Wrap(
          spacing: 6,
          runSpacing: 6,
          children: [
            for (final s in store.shortcuts)
              GestureDetector(
                onLongPress: () => _editShortcut(s),
                child: Tooltip(
                  message: s.label,
                  child: ActionChip(
                    label: _shortcutContent(s),
                    onPressed: () {
                      hub?.sendSelected(keyMsg(s.key, s.modifiers));
                    },
                  ),
                ),
              ),
            ActionChip(
              tooltip: '添加快捷键',
              label: const Icon(Icons.add, size: 18),
              onPressed: () => _editShortcut(null),
            ),
          ],
        ),
      ],
    ),
  );

  Future<void> _showVoiceHelp() async {
    await showDialog<void>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('语音自动发送说明'),
        content: const Text(
          'AgentsPads 只在输入框聚焦后检测到 Android 录音活动，或输入法明确声明 voice 模式时，才把本次输入视为语音；手打组字和粘贴不会单独触发。\n\n'
          '语音结束组字后会按设置等待，默认等待 0.5 秒。保留这段等待，是为了让带有 AI 自动整理排版功能的语音输入法有充裕时间完成文字的排列和重组；等待期间文字继续变化会重新计时。\n\n'
          'Android 会向普通应用隐藏录音来源。如果其他应用恰好在 AgentsPads 输入框聚焦期间开始录音，理论上仍可能被判为语音。',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx),
            child: const Text('知道了'),
          ),
        ],
      ),
    );
  }

  Future<void> _showShortcutPreview() async {
    final brightness = Theme.of(context).brightness;
    const icons = <(String, IconData)>[
      ('abc', Icons.abc),
      ('alternate_email', Icons.alternate_email),
      ('backspace', Icons.backspace),
      ('calculate', Icons.calculate),
      ('dialpad', Icons.dialpad),
      ('functions', Icons.functions),
      ('key', Icons.key),
      ('key_off', Icons.key_off),
      ('keyboard', Icons.keyboard),
      ('keyboard_alt', Icons.keyboard_alt),
      ('keyboard_arrow_down', Icons.keyboard_arrow_down),
      ('keyboard_arrow_left', Icons.keyboard_arrow_left),
      ('keyboard_arrow_right', Icons.keyboard_arrow_right),
      ('keyboard_arrow_up', Icons.keyboard_arrow_up),
      ('keyboard_backspace', Icons.keyboard_backspace),
      ('keyboard_capslock', Icons.keyboard_capslock),
      ('keyboard_command_key', Icons.keyboard_command_key),
      ('keyboard_control', Icons.keyboard_control),
      ('keyboard_control_key', Icons.keyboard_control_key),
      ('keyboard_double_arrow_down', Icons.keyboard_double_arrow_down),
      ('keyboard_double_arrow_left', Icons.keyboard_double_arrow_left),
      ('keyboard_double_arrow_right', Icons.keyboard_double_arrow_right),
      ('keyboard_double_arrow_up', Icons.keyboard_double_arrow_up),
      ('keyboard_hide', Icons.keyboard_hide),
      ('keyboard_option_key', Icons.keyboard_option_key),
      ('keyboard_return', Icons.keyboard_return),
      ('keyboard_tab', Icons.keyboard_tab),
      ('keyboard_voice', Icons.keyboard_voice),
      ('numbers', Icons.numbers),
      ('password', Icons.password),
      ('pin', Icons.pin),
      ('space_bar', Icons.space_bar),
      ('subdirectory_arrow_left', Icons.subdirectory_arrow_left),
      ('tag', Icons.tag),
      ('text_fields', Icons.text_fields),
      ('translate', Icons.translate),
    ];
    await showDialog<void>(
      context: context,
      builder: (ctx) => Dialog(
        key: const ValueKey('shortcut-preview'),
        child: Padding(
          padding: const EdgeInsets.all(20),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  const Text(
                    '快捷键图标预览',
                    style: TextStyle(fontSize: 18, fontWeight: FontWeight.w600),
                  ),
                  const Spacer(),
                  Text(
                    '临时测试',
                    style: TextStyle(
                      color: Theme.of(ctx).colorScheme.onSurfaceVariant,
                      fontSize: 12,
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 16),
              const Text('基础图标，不重复展示不同风格变体', style: TextStyle(fontSize: 12)),
              const SizedBox(height: 10),
              SizedBox(
                width: double.maxFinite,
                height: 320,
                child: GridView.builder(
                  itemCount: icons.length,
                  gridDelegate: const SliverGridDelegateWithMaxCrossAxisExtent(
                    maxCrossAxisExtent: 84,
                    mainAxisExtent: 68,
                    crossAxisSpacing: 8,
                    mainAxisSpacing: 8,
                  ),
                  itemBuilder: (_, index) {
                    final (name, icon) = icons[index];
                    return Tooltip(
                      message: name,
                      child: Container(
                        key: ValueKey('preview-icon-$name'),
                        decoration: BoxDecoration(
                          color: _controlBackground(brightness),
                          borderRadius: BorderRadius.circular(12),
                          border: Border.all(color: _controlBorder(brightness)),
                        ),
                        child: Column(
                          mainAxisAlignment: MainAxisAlignment.center,
                          children: [
                            Icon(
                              icon,
                              size: 22,
                              color: _controlForeground(brightness),
                            ),
                            const SizedBox(height: 4),
                            Padding(
                              padding: const EdgeInsets.symmetric(
                                horizontal: 4,
                              ),
                              child: Text(
                                name,
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                                style: TextStyle(
                                  color: _controlForeground(brightness),
                                  fontSize: 9,
                                ),
                              ),
                            ),
                          ],
                        ),
                      ),
                    );
                  },
                ),
              ),
              const SizedBox(height: 12),
              Align(
                alignment: Alignment.centerRight,
                child: TextButton(
                  onPressed: () => Navigator.pop(ctx),
                  child: const Text('关闭'),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  Future<void> _openAbout() async {
    await _openFloatingPanel(
      closeKey: const ValueKey('about-close'),
      closeTooltip: '关闭关于',
      builder: (ctx, _) => const Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text('关于 AgentsPads', style: TextStyle(fontSize: 20)),
          SizedBox(height: 16),
          Text(
            'AgentsPads 把 Android 手机变成 Windows 与 macOS 的局域网控制面。可以发送文字、快捷键和相对指针操作；连接不经过云端，也不需要账号。',
          ),
          SizedBox(height: 20),
          Text('撤回的限制', style: TextStyle(fontSize: 16)),
          SizedBox(height: 8),
          Text(
            '“撤回上次输入”会向电脑当前焦点应用发送 Ctrl+Z（Windows）或 Cmd+Z（macOS），并把上次发送的文字放回手机输入框。电脑端能否撤销取决于目标应用是否提供可用的撤销记录。\n\n'
            '终端和命令行通常不会把这个快捷键当作普通文本撤销，也无法可靠撤回已经执行的命令或输出。在普通可编辑文本框中使用最稳妥；终端里的内容请手动确认和处理。',
          ),
        ],
      ),
    );
  }

  final _ignoredPointerIds = <int>{};

  void _cancelTargetGesture() {
    _ignoredPointerIds.addAll(activePointerIds);
    activePointerIds.clear();
    _stopTrackPoint();
    pointerTimer?.cancel();
    pointerTimer = null;
    padLongPressTimer?.cancel();
    pointer.tick();
    pointer.buttons = 0;
    pointerCadence.reset();
    touchpad.cancel();
    leftDown = rightDown = pointerActive = false;
    if (mounted) setState(() {});
  }

  void _setPointerActive(PointerEvent event, bool active) {
    if (!active) _ignoredPointerIds.remove(event.pointer);
    active
        ? activePointerIds.add(event.pointer)
        : activePointerIds.remove(event.pointer);
    final next = activePointerIds.isNotEmpty;
    if (!next) _stopTrackPoint();
    if (next != pointerActive) setState(() => pointerActive = next);
  }

  void _emitTrackPoint() {
    final v = trackPointVec;
    if (v == null) return;
    _addPointer(v.dx * 0.14, v.dy * 0.14, _btns, 0);
  }

  void _holdTrackPoint(Offset local, BoxConstraints box) {
    trackPointVec = local - Offset(box.maxWidth / 2, box.maxHeight / 2);
    if (trackPointTimer?.isActive == true) return;
    _emitTrackPoint();
    trackPointTimer = Timer.periodic(_pointerPeriod, (_) => _emitTrackPoint());
  }

  void _stopTrackPoint() {
    trackPointVec = null;
    trackPointTimer?.cancel();
    trackPointTimer = null;
  }

  /// Same capsule chrome as device chips: fill + border + centered glow on one box.
  Widget _actionCapsule({
    Key? key,
    required bool selected,
    required String label,
    required VoidCallback onPressed,
    bool accentBorder = true,
    bool glow = true,
  }) {
    final brightness = Theme.of(context).brightness;
    final accent = accentBorder && selected;
    return GestureDetector(
      key: key,
      behavior: HitTestBehavior.opaque,
      onTap: onPressed,
      child: AnimatedContainer(
        duration: const Duration(milliseconds: 160),
        height: 34,
        alignment: Alignment.center,
        padding: const EdgeInsets.symmetric(horizontal: 4),
        decoration: BoxDecoration(
          color: _controlBackground(brightness, selected: selected),
          borderRadius: BorderRadius.circular(17),
          border: Border.all(
            color: accent ? _accentColor : _controlBorder(brightness),
          ),
          boxShadow: glow ? _lightSelectedGlow(selected) : null,
        ),
        child: Text(
          label,
          textAlign: TextAlign.center,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
          style: TextStyle(
            fontSize: 12,
            fontWeight: FontWeight.w700,
            color: _controlForeground(brightness, selected: selected),
          ),
        ),
      ),
    );
  }

  /// Light-mode only: centered accent outer glow (no offset / material elevation).
  List<BoxShadow>? _lightSelectedGlow(bool selected) {
    if (!selected || Theme.of(context).brightness != Brightness.light) {
      return null;
    }
    return [
      BoxShadow(
        color: _accentColor.withValues(alpha: 0.42),
        blurRadius: 5,
        spreadRadius: 0.25,
      ),
    ];
  }

  ButtonStyle _segmentStyle() {
    // Drop Material elevation — angled cast shadow ≠ centered outer glow.
    final base =
        Theme.of(context).segmentedButtonTheme.style ?? const ButtonStyle();
    return base.copyWith(
      elevation: const WidgetStatePropertyAll(0),
      shadowColor: const WidgetStatePropertyAll(Colors.transparent),
    );
  }

  Widget _shortcutContent(Shortcut shortcut) {
    if (shortcut.key == 'Enter') {
      return Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (shortcut.modifiers.contains('Shift')) ...[
            const Icon(Icons.keyboard_capslock, size: 16),
            const SizedBox(width: 2),
          ],
          const Icon(Icons.keyboard_return, size: 18),
        ],
      );
    }
    if (shortcut.key == 'Escape' && shortcut.modifiers.isEmpty) {
      return const Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(Icons.keyboard_alt_outlined, size: 16),
          SizedBox(width: 4),
          Text('Esc'),
        ],
      );
    }
    return Text(shortcut.label);
  }

  String get _connectionState => store.devices.isEmpty
      ? 'disconnected'
      : (_onlineCount == 0 ? 'connecting' : 'connected');

  String get _connectionText {
    if (store.deviceStripPlacement == 'top') return '$_onlineCount';
    return switch (_connectionState) {
      'connected' => '已连接 $_onlineCount 台',
      'connecting' => '连接中',
      _ => '未连接',
    };
  }

  Color get _connectionColor => switch (_connectionState) {
    'connected' => const Color(0xFF22B573),
    'connecting' => const Color(0xFFF5B700),
    _ => const Color(0xFFE5484D),
  };

  Color get _accentColor =>
      _themeSeed(store.themeColor, Theme.of(context).brightness);

  Color get _onAccentColor => onAccentForeground(
    _accentColor,
    Theme.of(context).brightness,
    store.themeColor,
  );

  // Impeller: Icon/Text `shadows` detach while scrolling. Soft-blur a twin glyph
  // instead so the glow still follows the icon outline (not a circle blob).
  Widget _connectionIcon() {
    final color = _connectionColor;
    return Stack(
      alignment: Alignment.center,
      clipBehavior: Clip.none,
      children: [
        ImageFiltered(
          imageFilter: ImageFilter.blur(
            sigmaX: 2.8,
            sigmaY: 2.8,
            tileMode: TileMode.decal,
          ),
          child: Icon(
            Icons.computer,
            size: 18,
            color: color.withValues(alpha: 0.7),
          ),
        ),
        Icon(
          Icons.computer,
          key: ValueKey('connection-icon-$_connectionState'),
          size: 18,
          color: color,
        ),
      ],
    );
  }

  Widget _deviceStrip({bool embedded = false}) {
    final strip = SizedBox(
      height: embedded ? 48 : 40,
      child: ListView.separated(
        key: const ValueKey('device-strip'),
        clipBehavior: Clip.none,
        padding: embedded
            ? const EdgeInsets.symmetric(horizontal: _deviceStripFadeWidth)
            : null,
        scrollDirection: Axis.horizontal,
        itemCount: store.devices.length,
        separatorBuilder: (_, _) => const SizedBox(width: 10),
        itemBuilder: (context, index) {
          final d = store.devices[index];
          final id = Hub.keyOf(d);
          final brightness = Theme.of(context).brightness;
          return Builder(
            builder: (capsuleContext) => GestureDetector(
              key: ValueKey('device-capsule-$id'),
              behavior: HitTestBehavior.opaque,
              onTap: () {
                store.selectDevice(d, !d.selected);
                _persist();
              },
              onLongPress: () => _showDeviceActions(d, capsuleContext),
              child: Center(
                child: AnimatedContainer(
                  duration: const Duration(milliseconds: 160),
                  height: 34,
                  padding: const EdgeInsets.symmetric(horizontal: 10),
                  decoration: BoxDecoration(
                    color: _controlBackground(brightness, selected: d.selected),
                    borderRadius: BorderRadius.circular(17),
                    border: Border.all(
                      color: d.selected
                          ? _accentColor
                          : _controlBorder(brightness),
                    ),
                    boxShadow: _lightSelectedGlow(d.selected),
                  ),
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Container(
                        key: ValueKey('device-toggle-$id'),
                        width: 9,
                        height: 9,
                        decoration: BoxDecoration(
                          color: d.selected
                              ? _accentColor
                              : _controlForeground(brightness),
                          shape: BoxShape.circle,
                        ),
                      ),
                      const SizedBox(width: 8),
                      Text(
                        d.name,
                        key: ValueKey('device-name-$id'),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: TextStyle(
                          color: _controlForeground(
                            brightness,
                            selected: d.selected,
                          ),
                          fontSize: 12,
                          fontWeight: FontWeight.w600,
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          );
        },
      ),
    );
    return Padding(
      padding: embedded
          ? EdgeInsets.zero
          : const EdgeInsets.fromLTRB(_pageGutter, 0, _pageGutter, 8),
      child: embedded ? _deviceStripEdgeFade(strip) : strip,
    );
  }

  Widget _deviceStripEdgeFade(Widget child) => ShaderMask(
    key: const ValueKey('device-strip-edge-fade'),
    blendMode: BlendMode.dstIn,
    shaderCallback: (bounds) {
      final edge = bounds.width <= 0
          ? 0.0
          : math.min(0.45, _deviceStripFadeWidth / bounds.width);
      return LinearGradient(
        begin: Alignment.centerLeft,
        end: Alignment.centerRight,
        colors: const [
          Colors.transparent,
          Colors.black,
          Colors.black,
          Colors.transparent,
        ],
        stops: [0, edge, 1 - edge, 1],
      ).createShader(bounds);
    },
    child: ClipRect(
      key: const ValueKey('device-strip-horizontal-clip'),
      clipper: _HorizontalOnlyClipper(
        1 / MediaQuery.devicePixelRatioOf(context),
      ),
      child: child,
    ),
  );

  Future<void> _showDeviceActions(Device d, BuildContext anchorContext) async {
    final index = store.devices.indexOf(d);
    if (index < 0) return;
    final anchor = anchorContext.findRenderObject() as RenderBox;
    final overlay = Overlay.of(context).context.findRenderObject() as RenderBox;
    final topLeft = anchor.localToGlobal(Offset.zero, ancestor: overlay);
    final rect = topLeft & anchor.size;
    const width = 132.0;
    const height = 44.0;
    const edge = 8.0;
    final maxLeft = (overlay.size.width - width - edge).clamp(
      edge,
      double.infinity,
    );
    final left = (rect.center.dx - width / 2).clamp(edge, maxLeft).toDouble();
    var top = rect.top - height - 6;
    if (top < edge) {
      top = (rect.bottom + 6).clamp(edge, overlay.size.height - height - edge);
    }
    final brightness = Theme.of(context).brightness;
    final action = await showGeneralDialog<String>(
      context: context,
      barrierDismissible: true,
      barrierLabel: '关闭设备操作',
      barrierColor: Colors.transparent,
      transitionDuration: const Duration(milliseconds: 120),
      pageBuilder: (dialogContext, _, _) => Material(
        type: MaterialType.transparency,
        child: Stack(
          children: [
            Positioned(
              left: left,
              top: top,
              width: width,
              height: height,
              child: Material(
                key: const ValueKey('device-actions'),
                color: _controlBackground(brightness),
                shape: StadiumBorder(
                  side: BorderSide(color: _controlBorder(brightness)),
                ),
                child: IconButtonTheme(
                  data: IconButtonThemeData(
                    style: IconButton.styleFrom(
                      foregroundColor: _controlForeground(brightness),
                      disabledForegroundColor: _controlForeground(brightness),
                      iconSize: 20,
                      minimumSize: const Size.square(40),
                      padding: EdgeInsets.zero,
                      tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                    ),
                  ),
                  child: Row(
                    mainAxisAlignment: MainAxisAlignment.spaceEvenly,
                    children: [
                      IconButton(
                        tooltip: '向左移动',
                        onPressed: index == 0
                            ? null
                            : () => Navigator.pop(dialogContext, 'left'),
                        icon: const Icon(Icons.chevron_left),
                      ),
                      IconButton(
                        tooltip: '向右移动',
                        onPressed: index == store.devices.length - 1
                            ? null
                            : () => Navigator.pop(dialogContext, 'right'),
                        icon: const Icon(Icons.chevron_right),
                      ),
                      IconButton(
                        tooltip: '删除设备',
                        onPressed: () => Navigator.pop(dialogContext, 'delete'),
                        icon: Icon(
                          Icons.delete_outline,
                          color: const Color(0xFFFF3B30),
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
    );
    switch (action) {
      case 'left':
        await _moveDevice(d, -1);
        break;
      case 'right':
        await _moveDevice(d, 1);
        break;
      case 'delete':
        await _confirmDelete(d);
        break;
    }
  }

  Future<void> _moveDevice(Device d, int offset) async {
    final from = store.devices.indexOf(d);
    final to = from + offset;
    if (from < 0 || to < 0 || to >= store.devices.length) return;
    store.devices.removeAt(from);
    store.devices.insert(to, d);
    await _persist();
  }

  Future<void> _confirmDelete(Device d) async {
    final deleted = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text('删除 ${d.name}?'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('取消'),
          ),
          TextButton(
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('删除'),
          ),
        ],
      ),
    );
    if (deleted != true) return;
    store.removeDevice(d);
    await _persist();
  }

  Widget _pad() {
    final dark = Theme.of(context).brightness == Brightness.dark;
    final brightness = dark ? Brightness.dark : Brightness.light;
    return _withWheel(
      dark,
      _ClaimParentScroll(
        child: Listener(
          onPointerSignal: (e) {
            if (e is PointerScrollEvent) {
              _addPointer(0, 0, 0, -e.scrollDelta.dy);
            }
          },
          onPointerDown: (e) {
            _applyPadActions(touchpad.down(e.pointer, e.localPosition));
            padLongPressTimer?.cancel();
            if (touchpad.pointerCount == 1) {
              padLongPressTimer = Timer(
                kLongPressTimeout,
                _armTouchpadLongPress,
              );
            }
          },
          onPointerMove: (e) {
            if (!_ignoredPointerIds.contains(e.pointer)) {
              _applyPadActions(touchpad.move(e.pointer, e.localPosition));
            }
          },
          onPointerUp: (e) {
            padLongPressTimer?.cancel();
            _applyPadActions(touchpad.up(e.pointer, e.localPosition));
          },
          onPointerCancel: (e) {
            padLongPressTimer?.cancel();
            _applyPadActions(touchpad.cancel());
          },
          child: DecoratedBox(
            key: const ValueKey('touchpad-surface'),
            decoration: BoxDecoration(
              color: _controlBackground(brightness),
              borderRadius: BorderRadius.circular(12),
              border: Border.all(color: _controlBorder(brightness)),
            ),
            child: const Center(
              child: Padding(
                padding: EdgeInsets.symmetric(horizontal: 4),
                child: FittedBox(
                  fit: BoxFit.scaleDown,
                  child: Text(
                    '单指：单击左键 / 长按拖动区选 / 长按松开右键\n'
                    '双指：点按右键 / 上下滑动滚轮',
                    textAlign: TextAlign.center,
                    style: TextStyle(fontSize: 12, height: 1.6),
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
      flat: true,
    );
  }

  void _armTouchpadLongPress() {
    if (!touchpad.armLongPress()) return;
    if (store.longPressHaptic) {
      HapticFeedback.selectionClick();
    }
  }

  void _applyPadActions(List<TouchpadAction> actions) {
    for (final action in actions) {
      if (action.immediate && action.wheel == 0) {
        _queueImmediatePointer(
          action.dx,
          action.dy,
          action.buttons,
          action.wheel,
        );
      } else {
        _addPointer(action.dx, action.dy, action.buttons, -action.wheel);
      }
    }
  }

  Widget _nub({required bool trackball}) {
    final dark = Theme.of(context).brightness == Brightness.dark;
    final colors = Theme.of(context).colorScheme;
    final chassis = dark ? const Color(0xFF2E2E2E) : const Color(0xFFF0F0F0);
    final chassisBorder = dark
        ? const Color(0xFF5C5C5C)
        : colors.outlineVariant;
    final keyUp = dark ? const Color(0xFF3A3A3C) : const Color(0xFFF2F2F2);
    final keyDown = dark ? const Color(0xFF2C2C2E) : const Color(0xFFB0B0B0);
    return DecoratedBox(
      key: const ValueKey('nub-panel'),
      decoration: BoxDecoration(
        color: chassis,
        borderRadius: BorderRadius.circular(14),
        border: Border.all(color: chassisBorder),
      ),
      child: Padding(
        padding: const EdgeInsets.all(8),
        child: _withWheel(
          dark,
          Row(
            children: [
              Expanded(
                child: _mouseKey('左键', leftDown, keyUp, keyDown, (v) {
                  setState(() => leftDown = v);
                  _emitButtons();
                }),
              ),
              const SizedBox(width: 10),
              trackball ? _trackballCap() : _trackPointCap(),
              const SizedBox(width: 10),
              Expanded(
                child: _mouseKey('右键', rightDown, keyUp, keyDown, (v) {
                  setState(() => rightDown = v);
                  _emitButtons();
                }),
              ),
            ],
          ),
        ),
      ),
    );
  }

  void _emitButtons() {
    _queueImmediatePointer(0, 0, _btns, 0);
  }

  Widget _mouseKey(
    String label,
    bool down,
    Color up,
    Color dn,
    void Function(bool) on,
  ) {
    return GestureDetector(
      onTapDown: (_) => on(true),
      onTapUp: (_) => on(false),
      onTapCancel: () => on(false),
      child: AnimatedContainer(
        duration: const Duration(milliseconds: 40),
        height: 56,
        alignment: Alignment.center,
        decoration: BoxDecoration(
          color: down ? dn : up,
          borderRadius: BorderRadius.circular(10),
          border: Border.all(color: Colors.black26),
          boxShadow: down
              ? const []
              : const [
                  BoxShadow(
                    color: Colors.black26,
                    blurRadius: 2,
                    offset: Offset(0, 2),
                  ),
                ],
        ),
        child: Text(label, style: const TextStyle(fontWeight: FontWeight.w600)),
      ),
    );
  }

  Widget _trackPointCap() {
    return SizedBox(
      key: const ValueKey('pointer-cap'),
      width: 60,
      height: 60,
      child: LayoutBuilder(
        builder: (context, box) {
          return Listener(
            key: const ValueKey('trackpoint-cap'),
            behavior: HitTestBehavior.opaque,
            onPointerDown: (e) => _holdTrackPoint(e.localPosition, box),
            onPointerMove: (e) {
              if (!_ignoredPointerIds.contains(e.pointer)) {
                _holdTrackPoint(e.localPosition, box);
              }
            },
            onPointerUp: (_) => _stopTrackPoint(),
            onPointerCancel: (_) => _stopTrackPoint(),
            child: DecoratedBox(
              decoration: BoxDecoration(
                shape: BoxShape.circle,
                gradient: const RadialGradient(
                  center: Alignment(-0.35, -0.4),
                  radius: 0.9,
                  colors: [
                    Color(0xFFFF6B6B),
                    Color(0xFFC62828),
                    Color(0xFF7F0000),
                  ],
                ),
                boxShadow: [
                  BoxShadow(
                    color: const Color(0xFFB71C1C).withValues(alpha: 0.55),
                    blurRadius: 8,
                    offset: const Offset(0, 3),
                  ),
                ],
                border: Border.all(color: const Color(0xFF4A0000)),
              ),
            ),
          );
        },
      ),
    );
  }

  Widget _trackballCap() {
    return SizedBox(
      key: const ValueKey('pointer-cap'),
      width: 60,
      height: 60,
      child: GestureDetector(
        onPanUpdate: (d) => _addPointer(d.delta.dx, d.delta.dy, _btns, 0),
        child: Stack(
          children: [
            Positioned.fill(
              child: DecoratedBox(
                decoration: BoxDecoration(
                  shape: BoxShape.circle,
                  gradient: const RadialGradient(
                    center: Alignment(-0.42, -0.5),
                    radius: 0.95,
                    colors: [
                      Color(0xFFFFB0A8),
                      Color(0xFFE53935),
                      Color(0xFF8E0000),
                    ],
                    stops: [0, 0.38, 1],
                  ),
                  border: Border.all(color: const Color(0xFF5A0000)),
                  boxShadow: const [
                    BoxShadow(
                      color: Colors.black45,
                      blurRadius: 8,
                      offset: Offset(0, 4),
                    ),
                  ],
                ),
              ),
            ),
            Positioned(
              left: 13,
              top: 10,
              child: Container(
                width: 17,
                height: 10,
                decoration: BoxDecoration(
                  color: Colors.white.withValues(alpha: 0.72),
                  borderRadius: BorderRadius.circular(10),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _withWheel(bool dark, Widget child, {bool flat = false}) {
    final wheel = flat ? _flatWheel(dark) : _wheel(dark);
    const gap = SizedBox(width: 8);
    return Row(
      children: store.wheelSide == 'left'
          ? [wheel, gap, Expanded(child: child)]
          : [Expanded(child: child), gap, wheel],
    );
  }

  Widget _wheel(bool dark) {
    final colors = Theme.of(context).colorScheme;
    return GestureDetector(
      onVerticalDragUpdate: (d) => _scrollWheel(d.delta.dy),
      child: Container(
        width: 26,
        height: 56,
        decoration: BoxDecoration(
          borderRadius: BorderRadius.circular(13),
          gradient: LinearGradient(
            begin: Alignment.topCenter,
            end: Alignment.bottomCenter,
            colors: dark
                ? const [
                    Color(0xFF5C5C5C),
                    Color(0xFF2E2E2E),
                    Color(0xFF5C5C5C),
                  ]
                : const [
                    Color(0xFFF0F0F0),
                    Color(0xFF9E9E9E),
                    Color(0xFFF0F0F0),
                  ],
          ),
          border: Border.all(
            color: dark ? const Color(0xFF5C5C5C) : colors.outlineVariant,
          ),
        ),
        child: Column(
          mainAxisAlignment: MainAxisAlignment.spaceEvenly,
          children: List.generate(
            7,
            (_) => Container(
              height: 2,
              margin: const EdgeInsets.symmetric(horizontal: 5),
              color: Colors.black26,
            ),
          ),
        ),
      ),
    );
  }

  Widget _flatWheel(bool dark) {
    final brightness = dark ? Brightness.dark : Brightness.light;
    return _ClaimParentScroll(
      child: Container(
        key: const ValueKey('touchpad-wheel'),
        width: 44,
        height: double.infinity,
        decoration: BoxDecoration(
          color: _controlBackground(brightness),
          borderRadius: BorderRadius.circular(10),
          border: Border.all(color: _controlBorder(brightness)),
        ),
        child: _WheelNub(
          enabled: !store.reduceMotion,
          onScroll: _scrollWheel,
          child: Icon(
            Icons.swap_vert,
            size: 20,
            color: _controlForeground(brightness),
          ),
        ),
      ),
    );
  }

  void _scrollWheel(double dy) {
    if (dy != 0) _addPointer(0, 0, _btns, -dy);
  }

  Future<void> _openSettings() async {
    unawaited(_loadAppIconState());
    if (!mounted) return;
    await _openFloatingPanel(
      scrollKey: const ValueKey('settings-scroll'),
      closeKey: const ValueKey('settings-close'),
      closeTooltip: '关闭设置',
      builder: (ctx, setSheet) {
        _refreshSettingsSheet = () {
          if (ctx.mounted) setSheet(() {});
        };
        return Theme(
          data: Theme.of(ctx).copyWith(
            segmentedButtonTheme: SegmentedButtonThemeData(
              style: _segmentStyle(),
            ),
          ),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              const Text('外观', style: TextStyle(fontSize: 18)),
              const SizedBox(height: 16),
              Row(
                mainAxisAlignment: MainAxisAlignment.spaceEvenly,
                children: [
                  _themePick(
                    ctx,
                    setSheet,
                    Icons.brightness_auto,
                    'system',
                    '系统',
                  ),
                  _themePick(ctx, setSheet, Icons.light_mode, 'light', '浅色'),
                  _themePick(ctx, setSheet, Icons.dark_mode, 'dark', '深色'),
                ],
              ),
              const SizedBox(height: 20),
              const Align(alignment: Alignment.centerLeft, child: Text('主题色')),
              const SizedBox(height: 8),
              FittedBox(
                fit: BoxFit.scaleDown,
                child: Row(
                  mainAxisAlignment: MainAxisAlignment.spaceBetween,
                  children: [
                    _colorPick(ctx, setSheet, 'monochrome', '黑白'),
                    _colorPick(ctx, setSheet, 'blue', '蓝色'),
                    _colorPick(ctx, setSheet, 'pink', '粉色'),
                    _colorPick(ctx, setSheet, 'green', '绿色'),
                    _colorPick(ctx, setSheet, 'gold', '金色'),
                    _colorPick(ctx, setSheet, 'red', '红色'),
                  ],
                ),
              ),
              const SizedBox(height: 20),
              const Align(alignment: Alignment.centerLeft, child: Text('桌面图标')),
              const SizedBox(height: 8),
              SizedBox(
                width: double.infinity,
                child: SegmentedButton<String>(
                  showSelectedIcon: false,
                  segments: const [
                    ButtonSegment(value: 'system', label: Text('跟随系统')),
                    ButtonSegment(value: 'white', label: Text('默认浅色')),
                    ButtonSegment(value: 'black', label: Text('沉稳深色')),
                  ],
                  selected: {store.appIcon},
                  onSelectionChanged: (s) async {
                    store.appIcon = s.first;
                    await _persist();
                    await _applyAppIcon(store.appIcon);
                    if (ctx.mounted) setSheet(() {});
                  },
                ),
              ),
              if (appIconRestartHintVisible(_appIconPending, _appIconCurrent))
                Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: _appIconRestartHint(ctx),
                ),
              const SizedBox(height: 24),
              Row(
                children: [
                  const Expanded(child: Text('连接后添加全部 IP')),
                  Transform.scale(
                    scale: 0.78,
                    alignment: Alignment.centerRight,
                    child: Switch(
                      key: const ValueKey('collect-all-ips'),
                      materialTapTargetSize: MaterialTapTargetSize.shrinkWrap,
                      value: store.collectAllIps,
                      onChanged: (v) async {
                        store.collectAllIps = v;
                        await _persist();
                        setSheet(() {});
                      },
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 24),
              const Align(
                alignment: Alignment.centerLeft,
                child: Text('已连接设备的快捷开关显示在什么地方'),
              ),
              const SizedBox(height: 8),
              SizedBox(
                key: const ValueKey('device-strip-placement'),
                width: double.infinity,
                child: SegmentedButton<String>(
                  showSelectedIcon: false,
                  segments: const [
                    ButtonSegment(value: 'input', label: Text('输入框下方')),
                    ButtonSegment(value: 'top', label: Text('顶部')),
                  ],
                  selected: {store.deviceStripPlacement},
                  onSelectionChanged: (s) async {
                    store.deviceStripPlacement = s.first;
                    await _persist();
                    setSheet(() {});
                  },
                ),
              ),
              const SizedBox(height: 24),
              const Align(
                alignment: Alignment.centerLeft,
                child: Text('是否限制同时激活的设备数量'),
              ),
              const SizedBox(height: 8),
              SizedBox(
                width: double.infinity,
                child: SegmentedButton<bool>(
                  key: const ValueKey('limit-active-devices'),
                  showSelectedIcon: false,
                  segments: const [
                    ButtonSegment(value: true, label: Text('单个')),
                    ButtonSegment(value: false, label: Text('不限')),
                  ],
                  selected: {store.limitActiveDevices},
                  onSelectionChanged: (values) async {
                    store.limitActiveDevices = values.first;
                    await _persist();
                    setSheet(() {});
                  },
                ),
              ),
              const SizedBox(height: 24),
              Row(
                children: [
                  const Text('语音自动发送延迟'),
                  IconButton(
                    tooltip: '语音自动发送说明',
                    onPressed: _showVoiceHelp,
                    visualDensity: VisualDensity.compact,
                    padding: EdgeInsets.zero,
                    constraints: const BoxConstraints.tightFor(
                      width: 28,
                      height: 28,
                    ),
                    iconSize: 18,
                    icon: const Icon(Icons.info_outline),
                  ),
                ],
              ),
              const SizedBox(height: 8),
              SizedBox(
                width: double.infinity,
                child: SegmentedButton<int>(
                  showSelectedIcon: false,
                  segments: const [
                    ButtonSegment(value: 0, label: Text('无延迟')),
                    ButtonSegment(value: 500, label: Text('0.5 秒')),
                    ButtonSegment(value: 1000, label: Text('1 秒')),
                    ButtonSegment(value: 1500, label: Text('1.5 秒')),
                  ],
                  selected: {store.voiceDelayMs},
                  onSelectionChanged: (s) async {
                    store.voiceDelayMs = s.first;
                    await _persist();
                    setSheet(() {});
                  },
                ),
              ),
              const SizedBox(height: 24),
              Row(
                children: [
                  const Expanded(child: Text('切换光标设备')),
                  const Text('首页显示'),
                  const SizedBox(width: 6),
                  Transform.scale(
                    scale: 0.78,
                    alignment: Alignment.centerRight,
                    child: Switch(
                      key: const ValueKey('home-pointer-quick-switch'),
                      materialTapTargetSize: MaterialTapTargetSize.shrinkWrap,
                      value: store.homePointerQuickSwitch,
                      onChanged: (v) async {
                        store.homePointerQuickSwitch = v;
                        await _persist();
                        setSheet(() {});
                      },
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 8),
              SizedBox(
                key: const ValueKey('settings-pointer-mode-sync'),
                width: double.infinity,
                child: SegmentedButton<String>(
                  showSelectedIcon: false,
                  segments: const [
                    ButtonSegment(value: 'trackpad', label: Text('触控板')),
                    ButtonSegment(value: 'trackball', label: Text('轨迹球')),
                    ButtonSegment(value: 'trackpoint', label: Text('小红点')),
                  ],
                  selected: {store.pointerMode},
                  onSelectionChanged: (s) async {
                    _stopTrackPoint();
                    store.pointerMode = s.first;
                    await _persist();
                    setSheet(() {});
                  },
                ),
              ),
              const SizedBox(height: 24),
              const Align(
                alignment: Alignment.centerLeft,
                child: Text('触控板大小'),
              ),
              const SizedBox(height: 8),
              SizedBox(
                width: double.infinity,
                child: SegmentedButton<String>(
                  showSelectedIcon: false,
                  segments: const [
                    ButtonSegment(value: 'small', label: Text('小')),
                    ButtonSegment(value: 'medium', label: Text('中')),
                    ButtonSegment(value: 'large', label: Text('大')),
                  ],
                  selected: {store.pointerSize},
                  onSelectionChanged: (s) async {
                    store.pointerSize = s.first;
                    await _persist();
                    setSheet(() {});
                  },
                ),
              ),
              const SizedBox(height: 24),
              Row(
                children: [
                  const Expanded(child: Text('长按轻震反馈')),
                  Transform.scale(
                    scale: 0.78,
                    alignment: Alignment.centerRight,
                    child: Switch(
                      key: const ValueKey('long-press-haptic'),
                      materialTapTargetSize: MaterialTapTargetSize.shrinkWrap,
                      value: store.longPressHaptic,
                      onChanged: (v) async {
                        store.longPressHaptic = v;
                        await _persist();
                        setSheet(() {});
                      },
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 24),
              const Align(alignment: Alignment.centerLeft, child: Text('滚轮位置')),
              const SizedBox(height: 8),
              SizedBox(
                width: double.infinity,
                child: SegmentedButton<String>(
                  showSelectedIcon: false,
                  segments: const [
                    ButtonSegment(value: 'left', label: Text('左侧')),
                    ButtonSegment(value: 'right', label: Text('右侧')),
                  ],
                  selected: {store.wheelSide},
                  onSelectionChanged: (s) async {
                    store.wheelSide = s.first;
                    await _persist();
                    setSheet(() {});
                  },
                ),
              ),
              const SizedBox(height: 16),
              const Align(
                alignment: Alignment.centerLeft,
                child: Text('滚轮方向反转'),
              ),
              const SizedBox(height: 8),
              Row(
                children: [
                  Expanded(
                    child: _platformSwitch(
                      key: const ValueKey('wheel-reverse-windows'),
                      label: 'Windows',
                      value: store.wheelReverseWindows,
                      onChanged: (v) async {
                        store.wheelReverseWindows = v;
                        await _persist();
                        setSheet(() {});
                      },
                    ),
                  ),
                  const SizedBox(width: 12),
                  Expanded(
                    child: _platformSwitch(
                      key: const ValueKey('wheel-reverse-mac'),
                      label: 'Mac',
                      value: store.wheelReverseMac,
                      onChanged: (v) async {
                        store.wheelReverseMac = v;
                        await _persist();
                        setSheet(() {});
                      },
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 24),
              const Align(alignment: Alignment.centerLeft, child: Text('指针速度')),
              const SizedBox(height: 8),
              _platformSpeedSlider(
                key: const ValueKey('pointer-speed-windows'),
                label: 'Windows',
                value: store.pointerSpeedWindows,
                gears: PadStore.pointerGears,
                onChanged: (v) async {
                  store.pointerSpeedWindows = v;
                  await _persist();
                  setSheet(() {});
                },
              ),
              const SizedBox(height: 8),
              _platformSpeedSlider(
                key: const ValueKey('pointer-speed-mac'),
                label: 'Mac',
                value: store.pointerSpeedMac,
                gears: PadStore.pointerGears,
                onChanged: (v) async {
                  store.pointerSpeedMac = v;
                  await _persist();
                  setSheet(() {});
                },
              ),
              const SizedBox(height: 16),
              const Align(alignment: Alignment.centerLeft, child: Text('滚轮速度')),
              const SizedBox(height: 8),
              _platformSpeedSlider(
                key: const ValueKey('wheel-speed-windows'),
                label: 'Windows',
                value: store.wheelSpeedWindows,
                gears: PadStore.wheelGearsWindows,
                onChanged: (v) async {
                  store.wheelSpeedWindows = v;
                  await _persist();
                  setSheet(() {});
                },
              ),
              const SizedBox(height: 8),
              _platformSpeedSlider(
                key: const ValueKey('wheel-speed-mac'),
                label: 'Mac',
                value: store.wheelSpeedMac,
                gears: PadStore.wheelGears,
                onChanged: (v) async {
                  store.wheelSpeedMac = v;
                  await _persist();
                  setSheet(() {});
                },
              ),
              const SizedBox(height: 24),
              const Align(
                alignment: Alignment.centerLeft,
                child: Text('指针发送频率'),
              ),
              const SizedBox(height: 8),
              SizedBox(
                width: double.infinity,
                child: SegmentedButton<int>(
                  key: const ValueKey('pointer-hz'),
                  showSelectedIcon: false,
                  segments: const [
                    ButtonSegment(value: 60, label: Text('60Hz')),
                    ButtonSegment(value: 120, label: Text('120Hz')),
                    ButtonSegment(value: 240, label: Text('240Hz')),
                  ],
                  selected: {store.pointerHz},
                  onSelectionChanged: (s) async {
                    store.pointerHz = s.first;
                    store.pointerHzManual = true;
                    _retunePointerCadence();
                    await _persist();
                    setSheet(() {});
                  },
                ),
              ),
              const SizedBox(height: 24),
              const Align(alignment: Alignment.centerLeft, child: Text('横屏布局')),
              const SizedBox(height: 8),
              SizedBox(
                key: const ValueKey('landscape-pointer-side-setting'),
                width: double.infinity,
                child: SegmentedButton<String>(
                  showSelectedIcon: false,
                  segments: const [
                    ButtonSegment(value: 'left', label: Text('触控在左')),
                    ButtonSegment(value: 'right', label: Text('触控在右')),
                  ],
                  selected: {store.landscapePointerSide},
                  onSelectionChanged: (s) async {
                    store.landscapePointerSide = s.first;
                    await _persist();
                    setSheet(() {});
                  },
                ),
              ),
              const SizedBox(height: 24),
              Row(
                children: [
                  const Expanded(child: Text('强制横屏')),
                  Transform.scale(
                    scale: 0.78,
                    alignment: Alignment.centerRight,
                    child: Switch(
                      key: const ValueKey('force-landscape'),
                      materialTapTargetSize: MaterialTapTargetSize.shrinkWrap,
                      value: store.forceLandscape,
                      onChanged: (v) async {
                        store.forceLandscape = v;
                        await _persist(layout: true);
                        setSheet(() {});
                      },
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 24),
              Row(
                children: [
                  const Expanded(child: Text('减弱动画效果')),
                  Transform.scale(
                    scale: 0.78,
                    alignment: Alignment.centerRight,
                    child: Switch(
                      key: const ValueKey('reduce-motion'),
                      materialTapTargetSize: MaterialTapTargetSize.shrinkWrap,
                      value: store.reduceMotion,
                      onChanged: (v) async {
                        store.reduceMotion = v;
                        await _persist();
                        setSheet(() {});
                      },
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 24),
              SwitchListTile(
                key: const ValueKey('diagnostic-toggle'),
                contentPadding: EdgeInsets.zero,
                title: const Text('诊断日志（仅本次运行）'),
                subtitle: const Text('开启时清空旧日志，重启后自动关闭'),
                value: DiagnosticLog.enabled,
                onChanged: (value) async {
                  await DiagnosticLog.setEnabled(value);
                  if (ctx.mounted) setSheet(() {});
                },
              ),
              Row(
                children: [
                  TextButton(
                    key: const ValueKey('diagnostic-view'),
                    onPressed: () async {
                      final log = await DiagnosticLog.read();
                      if (!ctx.mounted) return;
                      await showDialog<void>(
                        context: ctx,
                        builder: (context) => AlertDialog(
                          title: const Text('诊断日志'),
                          content: SizedBox(
                            width: double.maxFinite,
                            child: SingleChildScrollView(
                              child: SelectableText(log.isEmpty ? '暂无日志' : log),
                            ),
                          ),
                          actions: [
                            TextButton(
                              onPressed: () => Navigator.pop(context),
                              child: const Text('关闭'),
                            ),
                          ],
                        ),
                      );
                    },
                    child: const Text('查看日志'),
                  ),
                  TextButton(
                    key: const ValueKey('diagnostic-clear'),
                    onPressed: () async {
                      final cleared = await DiagnosticLog.clear();
                      if (ctx.mounted) {
                        ScaffoldMessenger.of(ctx).showSnackBar(
                          SnackBar(content: Text(cleared ? '日志已清空' : '清空失败')),
                        );
                      }
                    },
                    child: const Text('清空日志'),
                  ),
                ],
              ),
              const Divider(height: 1),
              const SizedBox(height: 16),
              Row(
                mainAxisAlignment: MainAxisAlignment.spaceBetween,
                children: [
                  InkWell(
                    key: const ValueKey('settings-version'),
                    borderRadius: BorderRadius.circular(8),
                    onTap: checkingUpdate
                        ? null
                        : () => _handleUpdateTap(
                            refresh: () {
                              if (ctx.mounted) setSheet(() {});
                            },
                          ),
                    child: Padding(
                      padding: const EdgeInsets.symmetric(vertical: 8),
                      child: Text(
                        '版本 $appVersion',
                        style: TextStyle(
                          color: _controlForeground(
                            Theme.of(ctx).brightness,
                          ).withValues(alpha: 0.6),
                          fontSize: 13,
                        ),
                      ),
                    ),
                  ),
                  TextButton.icon(
                    key: const ValueKey('settings-update-action'),
                    onPressed: checkingUpdate
                        ? null
                        : () => _handleUpdateTap(
                            refresh: () {
                              if (ctx.mounted) setSheet(() {});
                            },
                          ),
                    icon: checkingUpdate
                        ? const SizedBox.square(
                            dimension: 14,
                            child: CircularProgressIndicator(strokeWidth: 2),
                          )
                        : Icon(
                            pendingUpdateTag == null
                                ? Icons.refresh
                                : Icons.circle,
                            size: pendingUpdateTag == null ? 16 : 7,
                            color: pendingUpdateTag == null
                                ? null
                                : const Color(0xFF74B580),
                          ),
                    label: Text(
                      checkingUpdate
                          ? '检查中...'
                          : pendingUpdateTag == null
                          ? '检查更新'
                          : '可更新',
                      style: pendingUpdateTag == null
                          ? null
                          : const TextStyle(color: Color(0xFF74B580)),
                    ),
                  ),
                ],
              ),
            ],
          ),
        );
      },
    );
    _refreshSettingsSheet = null;
  }

  Future<void> _applyAppIcon(String iconPref) async {
    if (!mounted) return;
    final target = resolveAppIconTarget(
      iconPref,
      MediaQuery.platformBrightnessOf(context),
    );
    try {
      await const MethodChannel(
        'agentpad/ws',
      ).invokeMethod<bool>('setAppIcon', {'icon': target});
    } catch (error) {
      _showIconError(error);
    }
    await _loadAppIconState();
  }

  Future<void> _loadAppIconState() async {
    try {
      final raw = await const MethodChannel(
        'agentpad/ws',
      ).invokeMethod<Object?>('getAppIconState');
      if (!mounted || raw is! Map) return;
      final current = raw['current'];
      final pending = raw['pending'];
      setState(() {
        _appIconCurrent = current == 'black' ? 'black' : 'white';
        _appIconPending = pending == 'black'
            ? 'black'
            : pending == 'white'
            ? 'white'
            : null;
      });
      _refreshSettingsSheet?.call();
    } catch (_) {}
  }

  bool _iconActionRunning = false;

  void _showIconError(Object error) {
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(
        content: Text(
          '图标变更失败：${error is PlatformException ? error.message ?? error.code : error}',
        ),
      ),
    );
  }

  Future<void> _finishForAppIcon() => _runIconAction('finishWithAppIcon');

  Future<void> _restartForAppIcon() => _runIconAction('restartWithAppIcon');

  Future<void> _runIconAction(String method) async {
    if (_iconActionRunning) return;
    _iconActionRunning = true;
    try {
      await store.save();
      if (!mounted) return;
      await const MethodChannel(
        'agentpad/ws',
      ).invokeMethod<bool>('setAppIcon', {
        'icon': resolveAppIconTarget(
          store.appIcon,
          MediaQuery.platformBrightnessOf(context),
        ),
      });
      await const MethodChannel('agentpad/ws').invokeMethod<bool>(method);
    } catch (error) {
      _iconActionRunning = false;
      _showIconError(error);
      await _loadAppIconState();
    }
  }

  Widget _appIconRestartHint(BuildContext ctx) {
    final dark = Theme.of(ctx).brightness == Brightness.dark;
    final red = dark ? Colors.red.shade300 : Colors.red.shade700;
    final blue = dark ? Colors.blue.shade300 : Colors.blue.shade700;
    return Align(
      alignment: Alignment.centerLeft,
      child: Text.rich(
        key: const ValueKey('app-icon-restart-hint'),
        TextSpan(
          children: [
            TextSpan(
              text: '等下次重新启动软件时才会变更',
              style: TextStyle(color: red),
            ),
            const TextSpan(text: '（首页返回退出后生效，切后台不变） '),
            WidgetSpan(
              alignment: PlaceholderAlignment.baseline,
              baseline: TextBaseline.alphabetic,
              child: GestureDetector(
                key: const ValueKey('app-icon-restart'),
                onTap: _restartForAppIcon,
                child: Text(
                  '立即重启',
                  style: TextStyle(
                    color: blue,
                    decoration: TextDecoration.underline,
                    decorationColor: blue,
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }

  void _showUpdateNotice(String message) {
    updateNoticeTimer?.cancel();
    updateNoticeEntry?.remove();

    final brightness = Theme.of(context).brightness;
    final bottom = MediaQuery.paddingOf(context).bottom + 24;
    final overlay = Overlay.of(context, rootOverlay: true);
    late final OverlayEntry entry;
    entry = OverlayEntry(
      builder: (_) => Positioned(
        left: 16,
        right: 16,
        bottom: bottom,
        child: IgnorePointer(
          child: Center(
            child: Material(
              color: Colors.transparent,
              child: Container(
                padding: const EdgeInsets.symmetric(
                  horizontal: 16,
                  vertical: 12,
                ),
                decoration: BoxDecoration(
                  color: _controlBackground(brightness),
                  border: Border.all(color: _controlBorder(brightness)),
                  borderRadius: BorderRadius.circular(8),
                ),
                child: Text(
                  message,
                  style: TextStyle(
                    color: _controlForeground(brightness, selected: true),
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
    );
    overlay.insert(entry);
    updateNoticeEntry = entry;
    updateNoticeTimer = Timer(const Duration(seconds: 2), () {
      if (identical(updateNoticeEntry, entry)) {
        entry.remove();
        updateNoticeEntry = null;
      }
    });
  }

  void _refreshUpdateViews(VoidCallback? refresh) {
    if (mounted) setState(() {});
    refresh?.call();
  }

  void _handleUpdateTap({VoidCallback? refresh}) {
    if (pendingUpdateTag != null) {
      _showUpdateDialog();
      return;
    }
    unawaited(_checkAndroidUpdate(refresh: refresh));
  }

  Future<void> _checkAndroidUpdate({
    bool notify = true,
    VoidCallback? refresh,
  }) async {
    if (parseAppVersion(appVersion) == null) {
      if (notify) _showUpdateNotice('调试版不检查更新');
      return;
    }
    if (checkingUpdate) {
      if (notify) _showUpdateNotice('正在检查更新...');
      return;
    }
    checkingUpdate = true;
    _refreshUpdateViews(refresh);
    if (notify) _showUpdateNotice('正在检查更新...');

    HttpClient? client;
    try {
      client = HttpClient();
      client.connectionTimeout = const Duration(seconds: 8);
      AndroidUpdateInfo? latest;
      final sources = androidUpdateManifestUris();
      for (var index = 0; index < sources.length; index++) {
        final uri = sources[index];
        try {
          final request = await client.getUrl(uri);
          request.headers.set('User-Agent', 'AgentsPads-Android');
          request.headers.set('Accept', 'application/json');
          final response = await request.close().timeout(
            const Duration(seconds: 12),
          );
          if (response.statusCode != 200) {
            continue;
          }
          final raw = await response
              .transform(utf8.decoder)
              .join()
              .timeout(const Duration(seconds: 12));
          final candidate = parseAndroidUpdateManifest(raw);
          if (candidate == null) {
            continue;
          }
          if (index == 0) {
            latest = candidate;
            break;
          }
          latest = newerAndroidUpdate(latest, candidate);
        } catch (_) {}
      }
      if (latest == null) {
        if (mounted && notify) _showUpdateNotice('检查更新失败');
        unawaited(
          DiagnosticLog.record(
            DiagnosticKind.update,
            DiagnosticStage.check,
            DiagnosticResult.failed,
          ),
        );
        return;
      }
      final update = latest;
      const currentVer = appVersion;
      if (_isNewerVersion(update.version, currentVer)) {
        if (!mounted) return;
        setState(() {
          pendingUpdateTag = update.version;
          pendingUpdateBody = update.body;
          pendingUpdateApkUrl = update.apkUrl;
        });
        refresh?.call();
        if (notify) _showUpdateNotice('发现新版本 v${update.version}');
      } else if (mounted) {
        setState(() {
          pendingUpdateTag = null;
          pendingUpdateBody = null;
          pendingUpdateApkUrl = null;
        });
        refresh?.call();
        if (notify) _showUpdateNotice('当前已是最新版本 (v$currentVer)');
      }
    } catch (e) {
      if (mounted && notify) {
        _showUpdateNotice('检查更新出错: $e');
      }
    } finally {
      client?.close(force: true);
      checkingUpdate = false;
      _refreshUpdateViews(refresh);
    }
  }

  void _showUpdateDialog() {
    final tag = pendingUpdateTag;
    final url = pendingUpdateApkUrl;
    if (tag == null || url == null) return;
    final body = pendingUpdateBody ?? '';
    showDialog<void>(
      context: context,
      builder: (dCtx) => AlertDialog(
        title: Text('发现新版本 v$tag'),
        content: SingleChildScrollView(
          child: Text(body.isNotEmpty ? body : '点击下方按钮下载最新安装包覆盖更新。'),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(dCtx),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () {
              Navigator.pop(dCtx);
              const MethodChannel(
                'agentpad/ws',
              ).invokeMethod<bool>('openUrl', {'url': url});
            },
            child: const Text('下载并更新'),
          ),
        ],
      ),
    );
  }

  Widget _updateFooter() {
    final brightness = Theme.of(context).brightness;
    const updateGreen = Color(0xFF74B580);
    final available = pendingUpdateTag != null;
    return Padding(
      padding: const EdgeInsets.fromLTRB(_pageGutter, 0, _pageGutter, 8),
      child: Semantics(
        button: true,
        label: checkingUpdate
            ? '正在检查更新'
            : available
            ? '发现新版本'
            : '检查更新',
        child: Material(
          color: Colors.transparent,
          child: InkWell(
            key: const ValueKey('update-footer'),
            borderRadius: BorderRadius.circular(8),
            onTap: checkingUpdate ? null : () => _handleUpdateTap(),
            child: SizedBox(
              height: 24,
              child: Row(
                mainAxisAlignment: MainAxisAlignment.center,
                children: [
                  Text(
                    '版本 $appVersion',
                    style: TextStyle(
                      color: _controlForeground(
                        brightness,
                      ).withValues(alpha: 0.6),
                      fontSize: 12,
                    ),
                  ),
                  const SizedBox(width: 6),
                  if (checkingUpdate)
                    const SizedBox.square(
                      dimension: 11,
                      child: CircularProgressIndicator(strokeWidth: 1.5),
                    )
                  else if (available) ...[
                    Container(
                      width: 6,
                      height: 6,
                      decoration: const BoxDecoration(
                        color: updateGreen,
                        shape: BoxShape.circle,
                      ),
                    ),
                    const SizedBox(width: 4),
                    const Text(
                      '可更新',
                      style: TextStyle(color: updateGreen, fontSize: 12),
                    ),
                  ] else
                    Icon(
                      Icons.refresh,
                      size: 13,
                      color: _controlForeground(
                        brightness,
                      ).withValues(alpha: 0.45),
                    ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }

  bool _isNewerVersion(String remote, String current) =>
      isNewerAppVersion(remote, current);

  Future<void> _openFloatingPanel({
    required Widget Function(BuildContext ctx, StateSetter setSheet) builder,
    Key? scrollKey,
    Key? closeKey,
    String closeTooltip = '关闭',
  }) {
    return showGeneralDialog<void>(
      context: context,
      barrierDismissible: true,
      barrierLabel: 'dismiss',
      barrierColor: Colors.black.withValues(alpha: 0.45),
      transitionDuration: const Duration(milliseconds: 220),
      pageBuilder: (ctx, animation, secondaryAnimation) {
        return SafeArea(
          child: LayoutBuilder(
            builder: (ctx, constraints) {
              return Padding(
                padding: const EdgeInsets.symmetric(
                  horizontal: 20,
                  vertical: 24,
                ),
                child: Center(
                  child: ConstrainedBox(
                    constraints: BoxConstraints(
                      maxWidth: 480,
                      maxHeight: (constraints.maxHeight - 48).clamp(200, 1200),
                    ),
                    child: Material(
                      elevation: 8,
                      color: Theme.of(ctx).scaffoldBackgroundColor,
                      borderRadius: BorderRadius.circular(16),
                      clipBehavior: Clip.antiAlias,
                      child: StatefulBuilder(
                        builder: (ctx, setSheet) => Stack(
                          children: [
                            SingleChildScrollView(
                              key: scrollKey,
                              padding: const EdgeInsets.fromLTRB(
                                20,
                                56,
                                20,
                                28,
                              ),
                              child: builder(ctx, setSheet),
                            ),
                            Positioned(
                              top: 10,
                              right: 10,
                              child: IconButton.filledTonal(
                                key: closeKey,
                                tooltip: closeTooltip,
                                onPressed: () => Navigator.pop(ctx),
                                icon: const Icon(Icons.close),
                              ),
                            ),
                          ],
                        ),
                      ),
                    ),
                  ),
                ),
              );
            },
          ),
        );
      },
      transitionBuilder: (ctx, animation, secondaryAnimation, child) {
        final t = CurvedAnimation(
          parent: animation,
          curve: Curves.easeOutCubic,
          reverseCurve: Curves.easeInCubic,
        );
        return FadeTransition(
          opacity: t,
          child: ScaleTransition(
            scale: Tween<double>(begin: 0.94, end: 1).animate(t),
            child: child,
          ),
        );
      },
    );
  }

  Widget _speedGearSlider({
    required Key key,
    required double value,
    required List<double> gears,
    required ValueChanged<double> onChanged,
  }) {
    final labelColor = _controlForeground(Theme.of(context).brightness);
    var index = gears.indexOf(value);
    if (index < 0) {
      index = 0;
      var best = double.infinity;
      for (var i = 0; i < gears.length; i++) {
        final d = (gears[i] - value).abs();
        if (d < best) {
          best = d;
          index = i;
        }
      }
    }
    return Column(
      key: key,
      children: [
        SliderTheme(
          data: SliderTheme.of(context).copyWith(
            overlayShape: SliderComponentShape.noOverlay,
            trackHeight: 2.5,
            thumbShape: const RoundSliderThumbShape(enabledThumbRadius: 7),
            showValueIndicator: ShowValueIndicator.never,
          ),
          child: SizedBox(
            height: 28,
            child: Slider(
              value: index.toDouble(),
              min: 0,
              max: (gears.length - 1).toDouble(),
              divisions: gears.length - 1,
              label: '×${gears[index].round()}',
              onChanged: (v) => onChanged(gears[v.round()]),
            ),
          ),
        ),
        Padding(
          padding: const EdgeInsets.fromLTRB(10, 2, 10, 0),
          child: Row(
            mainAxisAlignment: MainAxisAlignment.spaceBetween,
            children: [
              for (final g in gears)
                Text(
                  '×${g.round()}',
                  style: TextStyle(fontSize: 11, color: labelColor),
                ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _platformSwitch({
    required Key key,
    required String label,
    required bool value,
    required ValueChanged<bool> onChanged,
  }) {
    return Row(
      key: key,
      children: [
        Expanded(child: Text(label)),
        Transform.scale(
          scale: 0.78,
          alignment: Alignment.centerRight,
          child: Switch(
            materialTapTargetSize: MaterialTapTargetSize.shrinkWrap,
            value: value,
            onChanged: onChanged,
          ),
        ),
      ],
    );
  }

  Widget _platformSpeedSlider({
    required Key key,
    required String label,
    required double value,
    required List<double> gears,
    required ValueChanged<double> onChanged,
  }) {
    return Column(
      key: key,
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(label),
        const SizedBox(height: 4),
        _speedGearSlider(
          key: ValueKey('$label-wheel-speed-slider'),
          value: value,
          gears: gears,
          onChanged: onChanged,
        ),
      ],
    );
  }

  Widget _themePick(
    BuildContext ctx,
    StateSetter setSheet,
    IconData icon,
    String value,
    String label,
  ) {
    final selected = store.theme == value;
    final brightness = Theme.of(ctx).brightness;
    return Column(
      children: [
        IconButton(
          isSelected: selected,
          style: IconButton.styleFrom(
            backgroundColor: selected
                ? _controlBackground(brightness, selected: true)
                : null,
            foregroundColor: selected
                ? _controlForeground(brightness, selected: true)
                : null,
          ),
          onPressed: () async {
            store.theme = value;
            await _persist(theme: true);
            if (ctx.mounted) setSheet(() {});
          },
          icon: Icon(icon),
        ),
        Text(label),
      ],
    );
  }

  Widget _colorPick(
    BuildContext ctx,
    StateSetter setSheet,
    String value,
    String label,
  ) {
    final selected = store.themeColor == value;
    final brightness = Theme.of(ctx).brightness;
    final color = _themeSeed(value, brightness);
    final edge = value == 'monochrome'
        ? Theme.of(ctx).colorScheme.onSurface
        : color;
    final selectedBorder = value == 'monochrome'
        ? edge
        : Color.lerp(color, Colors.white, 0.7)!;
    return Tooltip(
      message: label,
      child: InkResponse(
        key: ValueKey('theme-color-$value'),
        radius: 24,
        onTap: () async {
          store.themeColor = value;
          await _persist(theme: true);
          setSheet(() {});
        },
        child: SizedBox.square(
          dimension: 48,
          child: Center(
            child: AnimatedContainer(
              duration: const Duration(milliseconds: 160),
              width: 30,
              height: 30,
              clipBehavior: Clip.antiAlias,
              decoration: BoxDecoration(
                color: color,
                borderRadius: BorderRadius.circular(8),
                border: Border.all(
                  color: selected ? selectedBorder : _controlBorder(brightness),
                  width: 1,
                ),
              ),
              child: value == 'monochrome'
                  ? Padding(
                      padding: const EdgeInsets.all(1),
                      child: ClipRRect(
                        key: const ValueKey('theme-color-monochrome-inner'),
                        borderRadius: BorderRadius.circular(7),
                        child: CustomPaint(
                          key: const ValueKey('theme-color-monochrome-split'),
                          painter: const _MonochromeSwatchPainter(),
                        ),
                      ),
                    )
                  : null,
            ),
          ),
        ),
      ),
    );
  }

  Future<void> _openConnected() async {
    await showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      builder: (ctx) {
        return StatefulBuilder(
          builder: (ctx, setSheet) {
            _refreshConnectedSheet = () {
              if (ctx.mounted) setSheet(() {});
            };
            return Padding(
              padding: const EdgeInsets.fromLTRB(16, 16, 16, 24),
              child: SizedBox(
                height: MediaQuery.of(ctx).size.height * 0.7,
                child: Column(
                  children: [
                    Row(
                      children: [
                        const Text('已连接', style: TextStyle(fontSize: 18)),
                        const Spacer(),
                        IconButton(
                          tooltip: '刷新连接',
                          onPressed: () {
                            hub?.restart();
                            setSheet(() {});
                          },
                          icon: const Icon(Icons.refresh),
                        ),
                        IconButton(
                          tooltip: '扫码连接',
                          onPressed: () async {
                            final raw = await Navigator.of(ctx).push<String>(
                              MaterialPageRoute(
                                builder: (_) => const ScanPage(),
                              ),
                            );
                            if (raw == null) return;
                            final q = QrPayload.parse(raw);
                            if (q == null) {
                              if (ctx.mounted) {
                                ScaffoldMessenger.of(ctx).showSnackBar(
                                  const SnackBar(
                                    content: Text('无法识别二维码，请确认电脑端已更新'),
                                  ),
                                );
                              }
                              return;
                            }
                            store.upsert(
                              Device(
                                deviceId: q.deviceId,
                                name: q.name,
                                // Only the NIC shown on the QR; other NIC IPs
                                // come in only if collectAllIps after connect.
                                ips: [q.ip],
                                port: q.port,
                                os: q.os,
                                secret: q.secret,
                              ),
                            );
                            await _persist();
                            setSheet(() {});
                          },
                          icon: const Icon(Icons.qr_code_scanner),
                        ),
                        IconButton(
                          tooltip: '手动添加',
                          onPressed: () async {
                            await _manualAdd();
                            setSheet(() {});
                          },
                          icon: const Icon(Icons.add),
                        ),
                      ],
                    ),
                    Align(
                      alignment: Alignment.centerLeft,
                      child: Text(
                        '长按设备可删除，拖动可排序',
                        style: TextStyle(
                          color: Theme.of(ctx).colorScheme.onSurfaceVariant,
                        ),
                      ),
                    ),
                    const SizedBox(height: 8),
                    Expanded(
                      child: ReorderableListView(
                        buildDefaultDragHandles: false,
                        onReorderItem: (a, b) {
                          final x = store.devices.removeAt(a);
                          store.devices.insert(b, x);
                          _persist();
                          setSheet(() {});
                        },
                        children: [
                          for (final (index, d) in store.devices.indexed)
                            ListTile(
                              key: ValueKey(Hub.keyOf(d)),
                              leading: ReorderableDragStartListener(
                                key: ValueKey('device-drag-${Hub.keyOf(d)}'),
                                index: index,
                                child: Tooltip(
                                  message: '拖动排序 ${d.name}',
                                  child: const SizedBox.square(
                                    dimension: 48,
                                    child: Icon(Icons.drag_handle),
                                  ),
                                ),
                              ),
                              title: Row(
                                children: [
                                  Checkbox(
                                    value: d.selected,
                                    onChanged: (v) {
                                      store.selectDevice(d, v ?? true);
                                      _persist();
                                      setSheet(() {});
                                    },
                                  ),
                                  Expanded(child: Text(d.name)),
                                ],
                              ),
                              subtitle: Text(
                                '${d.ips.join(", ")}:${d.port}'
                                '${hub?.isOnline(d) == true ? " · 在线" : ""}'
                                '${d.needsPairing
                                    ? " · 被拒绝，请重新扫码或输入配对码"
                                    : !d.canAuthenticate
                                    ? " · 未配对，请扫码或输入配对码"
                                    : ""}',
                              ),
                              onLongPress: () async {
                                await _confirmDelete(d);
                                setSheet(() {});
                              },
                              trailing: IconButton(
                                icon: const Icon(Icons.edit),
                                onPressed: () async {
                                  await _editDevice(d);
                                  setSheet(() {});
                                },
                              ),
                            ),
                        ],
                      ),
                    ),
                  ],
                ),
              ),
            );
          },
        );
      },
    );
    _refreshConnectedSheet = null;
  }

  Future<void> _manualAdd() async {
    final name = TextEditingController();
    final ip = TextEditingController();
    final code = TextEditingController();
    final ok = await showDialog<bool>(
      context: context,
      builder: (c) => AlertDialog(
        title: const Text('手动添加'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            TextField(
              controller: name,
              decoration: const InputDecoration(labelText: '名字'),
            ),
            TextField(
              controller: ip,
              decoration: const InputDecoration(labelText: 'IP 或 IP:端口'),
            ),
            TextField(
              key: const ValueKey('manual-pair-code'),
              controller: code,
              keyboardType: TextInputType.number,
              maxLength: 4,
              decoration: const InputDecoration(
                labelText: '配对码',
                helperText: '电脑配对窗口显示的 4 位数字',
              ),
            ),
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(c, false),
            child: const Text('取消'),
          ),
          TextButton(
            onPressed: () => Navigator.pop(c, true),
            child: const Text('添加'),
          ),
        ],
      ),
    );
    if (ok != true) return;
    final hp = parseHostPort(ip.text);
    if (hp.host.isEmpty) return;
    final pairCode = code.text.trim();
    if (!isPairCode(pairCode)) {
      if (mounted) {
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(const SnackBar(content: Text('请输入电脑配对窗口显示的 4 位配对码')));
      }
      return;
    }
    store.upsert(
      Device(
        deviceId: '',
        name: name.text.trim().isEmpty ? hp.host : name.text.trim(),
        ips: [hp.host],
        port: hp.port,
        pairCode: pairCode,
      ),
    );
    await _persist();
  }

  Future<void> _editDevice(Device d) async {
    final name = TextEditingController(text: d.name);
    final port = TextEditingController(text: '${d.port}');
    final code = TextEditingController();
    final ipCtrls = [
      for (final ip in (d.ips.isEmpty ? [''] : d.ips))
        TextEditingController(text: ip),
    ];
    final ok = await showDialog<bool>(
      context: context,
      builder: (c) => StatefulBuilder(
        builder: (c, setD) => AlertDialog(
          title: const Text('编辑设备'),
          content: SizedBox(
            width: 320,
            child: SingleChildScrollView(
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  TextField(
                    controller: name,
                    decoration: const InputDecoration(labelText: '展示名'),
                  ),
                  const SizedBox(height: 8),
                  SelectableText(
                    d.deviceId.isEmpty
                        ? '设备 ID：连接后自动绑定'
                        : '设备 ID：${d.deviceId}',
                    style: TextStyle(
                      fontSize: 12,
                      color: Theme.of(c).colorScheme.onSurfaceVariant,
                    ),
                  ),
                  const SizedBox(height: 12),
                  TextField(
                    controller: port,
                    keyboardType: TextInputType.number,
                    decoration: const InputDecoration(labelText: '端口'),
                  ),
                  const SizedBox(height: 12),
                  const Text('IP 地址'),
                  const SizedBox(height: 4),
                  for (var i = 0; i < ipCtrls.length; i++)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 8),
                      child: Row(
                        children: [
                          Expanded(
                            child: TextField(
                              controller: ipCtrls[i],
                              decoration: InputDecoration(
                                labelText: 'IP ${i + 1}',
                              ),
                            ),
                          ),
                          IconButton(
                            tooltip: '删除',
                            onPressed: ipCtrls.length <= 1
                                ? null
                                : () => setD(() {
                                    ipCtrls.removeAt(i);
                                  }),
                            icon: const Icon(Icons.remove_circle_outline),
                          ),
                        ],
                      ),
                    ),
                  Align(
                    alignment: Alignment.centerLeft,
                    child: TextButton.icon(
                      onPressed: () => setD(() {
                        ipCtrls.add(TextEditingController());
                      }),
                      icon: const Icon(Icons.add, size: 18),
                      label: const Text('添加 IP'),
                    ),
                  ),
                  TextField(
                    key: const ValueKey('edit-pair-code'),
                    controller: code,
                    keyboardType: TextInputType.number,
                    maxLength: 4,
                    decoration: const InputDecoration(
                      labelText: '配对码（重新配对时填写）',
                      helperText: '电脑配对窗口显示的 4 位数字；留空则保持现有配对',
                    ),
                  ),
                ],
              ),
            ),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(c, false),
              child: const Text('取消'),
            ),
            TextButton(
              onPressed: () => Navigator.pop(c, true),
              child: const Text('保存'),
            ),
          ],
        ),
      ),
    );
    final nextName = name.text.trim();
    final nextPort = int.tryParse(port.text.trim()) ?? d.port;
    final nextIps = <String>[
      for (final c in ipCtrls)
        if (c.text.trim().isNotEmpty) c.text.trim(),
    ];
    final nextCode = code.text.trim();
    if (ok != true || nextIps.isEmpty) return;
    if (isPairCode(nextCode)) {
      d.pairCode = nextCode;
      d.needsPairing = false;
    }
    d.name = nextName.isEmpty ? d.name : nextName;
    d.ips = nextIps;
    d.port = nextPort > 0 ? nextPort : d.port;
    await _persist();
  }

  Future<void> _editShortcut(Shortcut? existing) async {
    final label = TextEditingController(text: existing?.label ?? '');
    final key = TextEditingController(text: existing?.key ?? 'Escape');
    var shift = existing?.modifiers.contains('Shift') ?? false;
    final ok = await showDialog<bool>(
      context: context,
      builder: (c) => StatefulBuilder(
        builder: (c, setD) => AlertDialog(
          title: Text(existing == null ? '添加快捷键' : '编辑快捷键'),
          content: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              TextField(
                controller: label,
                decoration: const InputDecoration(labelText: '按钮名'),
              ),
              TextField(
                controller: key,
                decoration: const InputDecoration(
                  labelText: '按键（Escape / Enter / …）',
                ),
              ),
              CheckboxListTile(
                title: const Text('Shift'),
                value: shift,
                onChanged: (v) => setD(() => shift = v ?? false),
              ),
            ],
          ),
          actions: [
            if (existing != null)
              TextButton(
                onPressed: () {
                  store.shortcuts.remove(existing);
                  Navigator.pop(c, true);
                },
                child: const Text('删除'),
              ),
            TextButton(
              onPressed: () => Navigator.pop(c, false),
              child: const Text('取消'),
            ),
            TextButton(
              onPressed: () => Navigator.pop(c, true),
              child: const Text('保存'),
            ),
          ],
        ),
      ),
    );
    if (ok != true) return;
    if (existing == null && label.text.trim().isNotEmpty) {
      store.shortcuts.add(
        Shortcut(
          id: DateTime.now().microsecondsSinceEpoch.toString(),
          label: label.text.trim(),
          key: key.text.trim().isEmpty ? 'Escape' : key.text.trim(),
          modifiers: [if (shift) 'Shift'],
        ),
      );
    } else if (existing != null && store.shortcuts.contains(existing)) {
      existing.label = label.text.trim().isEmpty
          ? existing.label
          : label.text.trim();
      existing.key = key.text.trim().isEmpty ? existing.key : key.text.trim();
      existing.modifiers = [if (shift) 'Shift'];
    }
    await _persist();
  }
}

ThemeData _appTheme(Brightness brightness) {
  final seed = _themeSeed('monochrome', brightness);
  final background = brightness == Brightness.dark
      ? const Color(0xFF141414)
      : const Color(0xFFF0F0F0);
  final generated = ColorScheme.fromSeed(
    seedColor: seed,
    brightness: brightness,
    dynamicSchemeVariant: DynamicSchemeVariant.monochrome,
  );
  final colors = generated.copyWith(
    primary: seed,
    onPrimary: ThemeData.estimateBrightnessForColor(seed) == Brightness.dark
        ? Colors.white
        : Colors.black,
    surface: background,
    surfaceDim: background,
    surfaceBright: background,
    surfaceContainerLowest: background,
    surfaceContainerLow: background,
    surfaceContainer: background,
    surfaceContainerHigh: background,
    surfaceContainerHighest: background,
    surfaceTint: Colors.transparent,
  );
  final segmentedStyle = ButtonStyle(
    backgroundColor: WidgetStateProperty.resolveWith(
      (states) => states.contains(WidgetState.selected)
          ? _controlBackground(brightness, selected: true)
          : _controlBackground(brightness),
    ),
    foregroundColor: WidgetStateProperty.resolveWith(
      (states) => states.contains(WidgetState.selected)
          ? _controlForeground(brightness, selected: true)
          : _controlForeground(brightness),
    ),
    side: WidgetStateProperty.resolveWith(
      (_) => BorderSide(color: _controlBorder(brightness), width: 1),
    ),
  );
  return ThemeData(
    useMaterial3: true,
    colorScheme: colors,
    scaffoldBackgroundColor: background,
    segmentedButtonTheme: SegmentedButtonThemeData(style: segmentedStyle),
    chipTheme: ChipThemeData(
      backgroundColor: _controlBackground(brightness),
      side: BorderSide(color: _controlBorder(brightness)),
      shape: const StadiumBorder(),
      labelStyle: TextStyle(color: _controlForeground(brightness)),
      iconTheme: IconThemeData(color: _controlForeground(brightness)),
    ),
  );
}

Color _themeSeed(String value, [Brightness brightness = Brightness.light]) =>
    switch (value) {
      'monochrome' =>
        brightness == Brightness.dark
            ? const Color(0xFFCCCCCC)
            : const Color(0xFF333333),
      'green' => const Color(0xFF74B580),
      'pink' => const Color(0xFFE9ACB7),
      'gold' => const Color(0xFFFFB000),
      'red' => const Color(0xFFF04444),
      _ => const Color(0xFF5EAFF9),
    };

Color onAccentForeground(
  Color accent,
  Brightness brightness,
  String themeColor,
) {
  if (themeColor == 'monochrome') {
    return ThemeData.estimateBrightnessForColor(accent) == Brightness.dark
        ? Colors.white
        : Colors.black;
  }
  return Colors.white;
}

Color _controlForeground(Brightness brightness, {bool selected = false}) =>
    brightness == Brightness.dark
    ? (selected ? const Color(0xFFF0F0F0) : const Color(0xFF6B6B6B))
    : (selected ? const Color(0xFF141414) : const Color(0xFF868686));

Color _controlBackground(Brightness brightness, {bool selected = false}) =>
    brightness == Brightness.dark
    ? (selected ? const Color(0xFF3C3B39) : const Color(0xFF2C2B28))
    : (selected ? const Color(0xFFF0F0F0) : const Color(0xFFD1D0CC));

Color _controlBorder(Brightness brightness) => brightness == Brightness.dark
    ? const Color(0xFF3D3C39)
    : const Color(0xFFBCBAB7);

class _MonochromeSwatchPainter extends CustomPainter {
  const _MonochromeSwatchPainter();

  @override
  void paint(Canvas canvas, Size size) {
    canvas.drawColor(Colors.white, BlendMode.src);
    canvas.drawPath(
      Path()
        ..moveTo(0, 0)
        ..lineTo(size.width, 0)
        ..lineTo(0, size.height)
        ..close(),
      Paint()..color = Colors.black,
    );
  }

  @override
  bool shouldRepaint(_MonochromeSwatchPainter oldDelegate) => false;
}

class ScanPage extends StatefulWidget {
  const ScanPage({super.key});

  @override
  State<ScanPage> createState() => _ScanPageState();
}

class _ScanPageState extends State<ScanPage> {
  final controller = MobileScannerController(formats: [BarcodeFormat.qrCode]);
  var done = false;
  var zoomRange = (min: 1.0, max: 1.0);
  var zoom = 1.0;

  @override
  void initState() {
    super.initState();
    unawaited(_loadZoomRange());
  }

  Future<void> _loadZoomRange() async {
    try {
      final r = await const MethodChannel(
        'agentpad/ws',
      ).invokeListMethod<double>('cameraZoomRange');
      if (!mounted || r == null || r.length != 2) return;
      setState(() => zoomRange = (min: r[0], max: r[1]));
    } catch (_) {}
  }

  Future<void> _setZoom(double ratio) async {
    setState(() => zoom = ratio);
    try {
      await controller.setZoomScale(
        linearZoomFor(ratio, zoomRange.min, zoomRange.max),
      );
    } catch (_) {}
  }

  @override
  void dispose() {
    controller.dispose();
    super.dispose();
  }

  void _onDetect(BarcodeCapture capture) {
    if (done) return;
    for (final b in capture.barcodes) {
      final v = (b.rawValue ?? b.displayValue)?.trim();
      if (v == null || v.isEmpty) continue;
      done = true;
      Navigator.of(context).pop(v);
      return;
    }
  }

  @override
  Widget build(BuildContext context) {
    final presets = zoomPresets(zoomRange.min, zoomRange.max);
    return Scaffold(
      appBar: AppBar(title: const Text('扫码')),
      body: Stack(
        children: [
          MobileScanner(
            controller: controller,
            onDetect: _onDetect,
            errorBuilder: (context, error) {
              return Center(
                child: Padding(
                  padding: const EdgeInsets.all(24),
                  child: Text('无法打开相机：${error.errorCode.name}\n请在系统设置里允许相机权限'),
                ),
              );
            },
            overlayBuilder: (context, constraints) {
              return const IgnorePointer(
                child: Center(
                  child: Text(
                    '对准电脑上的二维码',
                    style: TextStyle(
                      color: Colors.white,
                      fontSize: 16,
                      shadows: [Shadow(blurRadius: 8, color: Colors.black)],
                    ),
                  ),
                ),
              );
            },
          ),
          if (presets.length > 1)
            Positioned(
              left: 0,
              right: 0,
              bottom: 32,
              child: Center(
                child: DecoratedBox(
                  decoration: BoxDecoration(
                    color: Colors.black45,
                    borderRadius: BorderRadius.circular(24),
                  ),
                  child: Padding(
                    padding: const EdgeInsets.all(4),
                    child: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        for (final r in presets)
                          _ZoomChip(
                            label: zoomLabel(r),
                            selected: (zoom - r).abs() < 0.01,
                            onTap: () => _setZoom(r),
                          ),
                      ],
                    ),
                  ),
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/// CameraX linear zoom is linear in crop width between the min and max ratio.
double linearZoomFor(double ratio, double min, double max) {
  if (max <= min) return 0;
  return ((1 / min - 1 / ratio) / (1 / min - 1 / max)).clamp(0.0, 1.0);
}

List<double> zoomPresets(double min, double max) => [
  if (min < 0.95) min,
  for (final r in const [1.0, 2.0, 3.0, 5.0, 10.0])
    if (r >= min && r <= max) r,
];

String zoomLabel(double r) =>
    r < 1 ? '${r.toStringAsFixed(1)}x' : '${r.round()}x';

class _ZoomChip extends StatelessWidget {
  const _ZoomChip({
    required this.label,
    required this.selected,
    required this.onTap,
  });

  final String label;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 2),
      child: Material(
        color: selected ? Colors.white : Colors.transparent,
        shape: const CircleBorder(),
        child: InkWell(
          customBorder: const CircleBorder(),
          onTap: onTap,
          child: SizedBox.square(
            dimension: 40,
            child: Center(
              child: Text(
                label,
                style: TextStyle(
                  color: selected ? Colors.black : Colors.white,
                  fontSize: 13,
                  fontWeight: FontWeight.w600,
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _HorizontalOnlyClipper extends CustomClipper<Rect> {
  const _HorizontalOnlyClipper(this.pixelInset);

  final double pixelInset;

  @override
  Rect getClip(Size size) {
    final inset = math.min(pixelInset, size.width / 2);
    return Rect.fromLTRB(inset, -32, size.width - inset, size.height + 32);
  }

  @override
  bool shouldReclip(_HorizontalOnlyClipper oldClipper) =>
      pixelInset != oldClipper.pixelInset;
}

class _ClaimParentScroll extends StatelessWidget {
  const _ClaimParentScroll({required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    return RawGestureDetector(
      behavior: HitTestBehavior.opaque,
      gestures: {
        EagerGestureRecognizer:
            GestureRecognizerFactoryWithHandlers<EagerGestureRecognizer>(
              EagerGestureRecognizer.new,
              (_) {},
            ),
      },
      child: child,
    );
  }
}

/// Scroll-strip glyph: rides the finger on Y while pressed, springs back
/// (with overshoot) on release. Scroll deltas are forwarded regardless.
class _WheelNub extends StatefulWidget {
  const _WheelNub({
    required this.enabled,
    required this.onScroll,
    required this.child,
  });

  final bool enabled;
  final ValueChanged<double> onScroll;
  final Widget child;

  @override
  State<_WheelNub> createState() => _WheelNubState();
}

class _WheelNubState extends State<_WheelNub> {
  var dy = 0.0;
  var dragging = false;

  void _release() {
    if (dy == 0 && !dragging) return;
    setState(() {
      dragging = false;
      dy = 0;
    });
  }

  @override
  Widget build(BuildContext context) {
    return LayoutBuilder(
      builder: (context, box) {
        final limit = (box.maxHeight / 2 - 14).clamp(0.0, double.infinity);
        return Listener(
          behavior: HitTestBehavior.opaque,
          onPointerDown: (_) => setState(() => dragging = true),
          onPointerMove: (e) {
            if (!e.down) return;
            widget.onScroll(e.localDelta.dy);
            if (!widget.enabled) return;
            setState(() => dy = (dy + e.localDelta.dy).clamp(-limit, limit));
          },
          onPointerUp: (_) => _release(),
          onPointerCancel: (_) => _release(),
          child: Center(
            child: AnimatedContainer(
              duration: dragging || !widget.enabled
                  ? Duration.zero
                  : const Duration(milliseconds: 520),
              curve: Curves.elasticOut,
              transform: Matrix4.translationValues(0, dy, 0),
              child: widget.child,
            ),
          ),
        );
      },
    );
  }
}

/// Shrinks the child while a pointer is down and springs back on release.
class _PressScale extends StatefulWidget {
  const _PressScale({required this.enabled, required this.child});

  final bool enabled;
  final Widget child;

  @override
  State<_PressScale> createState() => _PressScaleState();
}

class _PressScaleState extends State<_PressScale> {
  var pressed = false;

  void _set(bool v) {
    if (pressed != v) setState(() => pressed = v);
  }

  @override
  Widget build(BuildContext context) {
    final down = pressed && widget.enabled;
    return Listener(
      behavior: HitTestBehavior.translucent,
      onPointerDown: (_) => _set(true),
      onPointerUp: (_) => _set(false),
      onPointerCancel: (_) => _set(false),
      child: AnimatedScale(
        scale: down ? 0.84 : 1,
        duration: down
            ? const Duration(milliseconds: 90)
            : const Duration(milliseconds: 420),
        curve: down ? Curves.easeOut : Curves.elasticOut,
        child: widget.child,
      ),
    );
  }
}
