import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:web_socket_channel/web_socket_channel.dart';

import 'diagnostic_log.dart';
import 'protocol.dart';
import 'store.dart';

typedef OnHub = void Function();

class Hub {
  Hub(this.store, {this.onChange, this.onTargetsChanged, this.active = true}) {
    store.onTargetsChanged = _targetsChanged;
  }

  final OnHub? onTargetsChanged;

  void _targetsChanged(List<Device> before, List<Device> next) {
    for (final d in before) {
      if (next.any((n) => _samePc(d, n))) continue;
      for (final e in links.entries) {
        if (!online.contains(e.key) || !_samePc(d, e.value.device)) continue;
        final link = e.value;
        link._wheelRemainder = 0;
        if (!link.sendPointer(0, 0, 0, 0, immediate: true)) {
          link.sendFast(pointerMsg(0, 0, 0, 0));
        }
        break;
      }
    }
    onTargetsChanged?.call();
  }

  final PadStore store;
  final OnHub? onChange;
  final Map<String, PcLink> links = {};
  final Set<String> online = {};
  bool active;

  void setActive(bool value) {
    if (active == value) return;
    active = value;
    if (active) {
      sync();
    } else {
      _stopLinks();
      _notify();
    }
  }

  void restart() {
    _stopLinks();
    sync();
  }

  /// UI 回调异常不得打断连接循环。
  void _notify() {
    try {
      onChange?.call();
    } catch (_) {}
  }

  static String keyOf(Device d) => d.deviceId.isNotEmpty
      ? d.deviceId
      : (d.ips.isEmpty ? d.name : '${d.ips.first}:${d.port}');

  void sync() {
    if (!active) return;
    final keys = {for (final d in store.devices) keyOf(d)};
    for (final k in links.keys.toList()) {
      if (!keys.contains(k)) {
        links.remove(k)?.stop();
        online.remove(k);
      }
    }
    for (final d in store.devices) {
      final k = keyOf(d);
      final existing = links[k];
      if (existing == null || existing._stop) {
        links.remove(k);
        online.remove(k);
        existing?.stop();
        links[k] = PcLink(this, d, k)..start();
      } else {
        existing.device = d;
      }
    }
    _notify();
  }

  /// Wake a backoff wait. A live session is left alone.
  void nudge() {
    if (!active) return;
    for (final link in links.values.toList()) {
      link.nudge();
    }
  }

  Future<bool> sendTo(Device d, String json) async {
    for (final e in links.entries) {
      if (!online.contains(e.key)) continue;
      if (!_samePc(d, e.value.device)) continue;
      return e.value.send(json);
    }
    return false;
  }

  bool isOnline(Device d) => links.entries.any(
    (e) => online.contains(e.key) && _samePc(d, e.value.device),
  );

  static bool _samePc(Device a, Device b) {
    if (identical(a, b)) return true;
    if (a.deviceId.isNotEmpty && b.deviceId.isNotEmpty) {
      return a.deviceId == b.deviceId;
    }
    return a.ips.toSet().intersection(b.ips.toSet()).isNotEmpty;
  }

  Future<bool> sendSelected(String json) async {
    var any = false;
    for (final d in store.activeDevices.toList()) {
      if (await sendTo(d, json)) any = true;
    }
    return any;
  }

  void sendSelectedFast(String json) {
    for (final d in store.activeDevices.toList()) {
      sendToFast(d, json);
    }
  }

  void sendPointer(
    double dx,
    double dy,
    int buttons,
    double wheel, {
    bool immediate = false,
  }) {
    for (final d in store.activeDevices.toList()) {
      for (final e in links.entries) {
        if (!online.contains(e.key)) continue;
        if (!_samePc(d, e.value.device)) continue;
        final link = e.value;
        final pointerSpeed = store.pointerSpeedFor(d.os);
        final sign = store.wheelReverseFor(d.os) ? -1 : 1;
        final scaledWheel =
            wheel * store.wheelSpeedFor(d.os) * sign + link._wheelRemainder;
        final outWheel = scaledWheel.truncate();
        link._wheelRemainder = scaledWheel - outWheel;
        if (!link.sendPointer(
          dx * pointerSpeed,
          dy * pointerSpeed,
          buttons,
          outWheel,
          immediate: immediate,
        )) {
          link.sendFast(
            pointerMsg(dx * pointerSpeed, dy * pointerSpeed, buttons, outWheel),
          );
        }
        break;
      }
    }
  }

  void sendToFast(Device d, String json) {
    for (final e in links.entries) {
      if (!online.contains(e.key)) continue;
      if (!_samePc(d, e.value.device)) continue;
      if (e.value.hasPointerPump) return;
      e.value.sendFast(json);
      return;
    }
  }

  void dispose() {
    store.onTargetsChanged = null;
    active = false;
    _stopLinks();
  }

  void _stopLinks() {
    for (final l in links.values) {
      l.stop();
    }
    links.clear();
    online.clear();
  }
}

class PcLink {
  PcLink(this.hub, this.device, this.key);

  final Hub hub;
  String key;
  Device device;
  static int fallbackSockets = 0;

  /// No inbound frame for two of these periods closes the socket.
  @visibleForTesting
  static Duration silenceLimit = const Duration(seconds: 10);
  bool _stop = false;
  static int _nextTransportId = 0;
  Timer? _retryTimer;
  Completer<void>? _retryDone;
  double _wheelRemainder = 0;
  Future<bool> Function(String)? _send;
  void Function(String)? _sendFast;
  void Function(double, double, int, int, bool)? _sendPointer;
  Future<void> Function()? _close;
  int _attempt = 0;
  bool _identified = false;
  bool _answered = false;
  bool _waitingPong = false;
  String? _pendingReply;
  Timer? _silence;

  void start() {
    _stop = false;
    () async {
      while (!_stop) {
        var connected = false;
        for (final ip in device.ips) {
          if (_stop || !device.canAuthenticate) break;
          connected = await _try(ip, device.port);
          if (connected) break;
        }
        if (_stop) return;
        if (!connected && !_stop) {
          final done = _retryDone = Completer<void>();
          _retryTimer = Timer(const Duration(seconds: 2), done.complete);
          await done.future;
          _retryDone = null;
          _retryTimer = null;
        }
      }
    }();
  }

  void stop() {
    _stop = true;
    _silence?.cancel();
    _silence = null;
    _retryTimer?.cancel();
    final done = _retryDone;
    _retryDone = null;
    _retryTimer = null;
    if (done != null && !done.isCompleted) done.complete();
    _wheelRemainder = 0;
    final c = _close;
    _close = null;
    _send = null;
    _sendFast = null;
    _sendPointer = null;
    if (identical(hub.links[key], this)) hub.online.remove(key);
    c?.call();
  }

  void nudge() {
    if (_stop) return;
    final done = _retryDone;
    if (done == null || done.isCompleted) return;
    _retryTimer?.cancel();
    _retryTimer = null;
    _retryDone = null;
    done.complete();
  }

  bool get hasPointerPump => _sendPointer != null;

  Future<bool> send(String json) async {
    final s = _send;
    final ok = s != null && await s(json);
    if (DiagnosticLog.enabled && !hasPointerPump) {
      unawaited(
        DiagnosticLog.record(
          DiagnosticLog.classify(json),
          DiagnosticStage.send,
          ok ? DiagnosticResult.ok : DiagnosticResult.failed,
        ),
      );
    }
    return ok;
  }

  void sendFast(String json) {
    final f = _sendFast;
    if (f != null) {
      f(json);
      if (DiagnosticLog.enabled && !hasPointerPump) {
        unawaited(
          DiagnosticLog.record(
            DiagnosticLog.classify(json),
            DiagnosticStage.send,
            DiagnosticResult.ok,
          ),
        );
      }
      return;
    }
    unawaited(send(json));
  }

  bool sendPointer(
    double dx,
    double dy,
    int buttons,
    int wheel, {
    bool immediate = false,
  }) {
    final f = _sendPointer;
    if (f == null) return false;
    f(dx, dy, buttons, wheel, immediate);
    return true;
  }

  Future<bool> _try(String host, int port) async {
    final id = '$key:${_nextTransportId++}';
    final attempt = ++_attempt;
    _identified = false;
    _answered = false;
    _pendingReply = null;
    var session = false;
    Timer? handshake;
    void onText(String raw) {
      if (attempt == _attempt) onServerMessage(raw);
    }

    void awaitIdentity() {
      handshake = Timer(const Duration(seconds: 5), () {
        if (attempt == _attempt && !_identified) unawaited(_close?.call());
      });
    }

    unawaited(
      DiagnosticLog.record(
        DiagnosticKind.connection,
        DiagnosticStage.start,
        DiagnosticResult.active,
      ),
    );
    try {
      NativeWs? native;
      try {
        _close = () => NativeWs.closeId(id);
        native = await NativeWs.connect(id, host, port, onText: onText);
        if (_stop) {
          await native?.close();
          return false;
        }
        if (native != null) {
          _send = native.send;
          _sendFast = native.sendFast;
          _sendPointer = native.addPointer;
          _close = native.close;
          awaitIdentity();
          _flushReply();
          session = true;
          unawaited(
            DiagnosticLog.record(
              DiagnosticKind.connection,
              DiagnosticStage.start,
              DiagnosticResult.ok,
            ),
          );
          await native.done;
          return _identified;
        }
      } catch (_) {
        // 原生连接已建立后出错：交给 finally 关闭，不再叠一条备用连接。
        if (native != null) return _identified;
      }
      if (_stop || !await NativeWs.transportVisible) return false;
      fallbackSockets++;
      final ch = WebSocketChannel.connect(Uri.parse('ws://$host:$port'));
      _close = () async => ch.sink.close();
      await ch.ready.timeout(const Duration(seconds: 4));
      if (_stop) {
        await ch.sink.close();
        return false;
      }
      _send = (String j) async {
        ch.sink.add(j);
        return true;
      };
      _sendFast = (String j) => ch.sink.add(j);
      awaitIdentity();
      _flushReply();
      session = true;
      unawaited(
        DiagnosticLog.record(
          DiagnosticKind.connection,
          DiagnosticStage.start,
          DiagnosticResult.ok,
        ),
      );
      await for (final msg in ch.stream) {
        if (msg is String) onText(msg);
      }
      return _identified;
    } catch (_) {
      return _identified;
    } finally {
      handshake?.cancel();
      _silence?.cancel();
      _silence = null;
      _identified = false;
      _answered = false;
      _pendingReply = null;
      unawaited(
        DiagnosticLog.record(
          DiagnosticKind.connection,
          DiagnosticStage.stop,
          session ? DiagnosticResult.ok : DiagnosticResult.failed,
        ),
      );
      if (identical(hub.links[key], this)) hub.online.remove(key);
      final close = _close;
      _close = null;
      unawaited(close?.call());
      _wheelRemainder = 0;
      _send = null;
      _sendFast = null;
      _sendPointer = null;
      _close = null;
      if (identical(hub.links[key], this)) hub._notify();
    }
  }

  /// 挑战可能早于原生 connect 返回到达，先缓存应答。
  void _reply(String json) {
    final send = _send;
    if (send == null) {
      _pendingReply = json;
    } else {
      unawaited(send(json));
    }
  }

  void _flushReply() {
    final json = _pendingReply;
    _pendingReply = null;
    if (json != null) _reply(json);
  }

  bool _isOtherPc(String did) =>
      did.isEmpty || (device.deviceId.isNotEmpty && device.deviceId != did);

  /// 仅当身份确认后才上线；已识别过的设备换了 device_id（如旧 IP 被另一台电脑占用）一律拒绝，
  /// 且不向它发送任何凭据。
  @visibleForTesting
  void onServerMessage(String raw) {
    if (_stop) return;
    _waitingPong = false;
    if (DiagnosticLog.enabled) {
      unawaited(
        DiagnosticLog.record(
          DiagnosticLog.classify(raw),
          DiagnosticStage.receive,
          DiagnosticResult.ok,
        ),
      );
    }
    try {
      final v = jsonDecode(raw);
      if (v is! Map) return;
      if (v['type'] == 'challenge') {
        final nonce = v['nonce'] as String? ?? '';
        if (_answered ||
            nonce.isEmpty ||
            _isOtherPc(v['device_id'] as String? ?? '') ||
            !device.canAuthenticate) {
          unawaited(_close?.call());
          return;
        }
        _answered = true;
        _reply(
          device.pairCode.isNotEmpty
              ? pairMsg(hub.store.clientId, 'Android', device.pairCode)
              : helloMsg(
                  hub.store.clientId,
                  'Android',
                  authTag(device.secret, nonce),
                ),
        );
      } else if (v['type'] == 'auth_failed') {
        if (!_answered) return;
        device.pairCode = '';
        device.needsPairing = true;
        unawaited(_close?.call());
        hub._notify();
      } else if (v['type'] == 'connected') {
        final did = v['device_id'] as String? ?? '';
        if (!_answered || _isOtherPc(did)) {
          unawaited(_close?.call());
          return;
        }
        final secret = v['secret'];
        if (secret is String && secret.isNotEmpty) device.secret = secret;
        device.pairCode = '';
        device.deviceId = did;
        final os = v['os'] as String? ?? '';
        if (os.isNotEmpty) device.os = os;
        // Display name is user-owned; never overwrite from the PC hostname.
        if (hub.store.collectAllIps && v['ips'] is List) {
          for (final e in v['ips'] as List) {
            final ip = e.toString();
            if (ip.isNotEmpty && !device.ips.contains(ip)) device.ips.add(ip);
          }
        }
        _identified = true;
        _armSilence();
        hub.store.upsert(device);
        device = hub.store.devices.firstWhere(
          (d) => d.deviceId == did,
          orElse: () => device,
        );
        _rekey(Hub.keyOf(device));
        hub.online.add(key);
        hub.store.save();
        hub._notify();
      }
    } catch (_) {}
  }

  void _armSilence() {
    _silence?.cancel();
    _waitingPong = false;
    final limit = silenceLimit;
    _silence = Timer.periodic(limit, (_) {
      if (_stop || !_identified) return;
      if (_waitingPong) {
        unawaited(_close?.call());
        return;
      }
      _waitingPong = true;
      _reply(pingMsg());
    });
  }

  void _rekey(String next) {
    if (next == key) return;
    if (identical(hub.links[key], this)) hub.links.remove(key);
    hub.online.remove(key);
    key = next;
    final previous = hub.links[key];
    if (previous != null && !identical(previous, this)) previous.stop();
    hub.links[key] = this;
  }
}

class NativeWs {
  NativeWs._(this.id, this._done);
  final String id;
  final Completer<void> _done;

  Future<void> get done => _done.future;

  static const _m = MethodChannel('agentpad/ws');
  static const _e = EventChannel('agentpad/ws_events');
  static Stream<dynamic>? _events;
  static StreamSubscription<dynamic>? _sub;
  static final _waiters = <String, Completer<void>>{};
  static final _texts = <String, void Function(String)>{};
  static void Function()? _onInputBackspace;

  static set inputBackspaceHandler(void Function()? handler) {
    _onInputBackspace = handler;
    if (handler != null) _listen(restart: true);
  }

  static Future<bool> voiceEvidence() async {
    try {
      return await _m.invokeMethod<bool>('voiceEvidence') ?? false;
    } catch (_) {
      return false;
    }
  }

  static Future<void> resetVoiceEvidence() async {
    try {
      await _m.invokeMethod<void>('resetVoiceEvidence');
    } catch (_) {}
  }

  static void _listen({bool restart = false}) {
    if (restart) {
      unawaited(_sub?.cancel());
      _sub = null;
      _events = null;
    }
    _events ??= _e.receiveBroadcastStream();
    _sub ??= _events!.listen((ev) {
      if (ev is! Map) return;
      if (ev['event'] == 'inputBackspace') {
        _onInputBackspace?.call();
        return;
      }
      final id = ev['id'] as String?;
      if (id == null) return;
      if (ev['event'] == 'close') {
        _texts.remove(id);
        _waiters.remove(id)?.complete();
      } else if (ev['event'] == 'text') {
        final data = ev['data'] as String?;
        if (data != null) _texts[id]?.call(data);
      }
    });
  }

  static Future<bool> get transportVisible async {
    try {
      return await _m.invokeMethod<bool>('wsVisible') ?? true;
    } catch (_) {
      return true;
    }
  }

  static Future<NativeWs?> connect(
    String id,
    String host,
    int port, {
    void Function(String)? onText,
  }) async {
    _listen();
    if (onText != null) _texts[id] = onText;
    final done = Completer<void>();
    _waiters[id] = done;
    try {
      final ok = await _m.invokeMethod<bool>('connect', {
        'id': id,
        'host': host,
        'port': port,
      });
      if (ok != true) {
        _forget(id, done);
        return null;
      }
    } on MissingPluginException {
      _forget(id, done);
      return null;
    } on PlatformException {
      _forget(id, done);
      return null;
    }
    return NativeWs._(id, done);
  }

  static void _forget(String id, Completer<void> done) {
    if (!identical(_waiters[id], done)) return;
    _texts.remove(id);
    _waiters.remove(id);
  }

  static Future<double?> displayRefreshHz() async {
    try {
      final v = await _m.invokeMethod<num>('displayRefreshHz');
      return v?.toDouble();
    } catch (_) {
      return null;
    }
  }

  Future<bool> send(String text) async {
    try {
      return await _m.invokeMethod<bool>('send', {'id': id, 'text': text}) ==
          true;
    } catch (_) {
      return false;
    }
  }

  void sendFast(String text) {
    unawaited(send(text));
  }

  void addPointer(
    double dx,
    double dy,
    int buttons,
    int wheel,
    bool immediate,
  ) {
    unawaited(
      _m.invokeMethod('pointer', {
        'id': id,
        'dx': dx,
        'dy': dy,
        'buttons': buttons,
        'wheel': wheel,
        'immediate': immediate,
      }),
    );
  }

  Future<void> close() => closeId(id);

  static Future<void> closeId(String id) async {
    _texts.remove(id);
    _waiters.remove(id)?.complete();
    try {
      await _m.invokeMethod('close', {'id': id});
    } catch (_) {}
  }
}

bool sendClearsInput(bool anySendSucceeded) => anySendSucceeded;
