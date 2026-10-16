import 'dart:convert';

import 'package:agentpad/hub.dart';
import 'package:agentpad/protocol.dart';
import 'package:agentpad/store.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

class RecordingLink extends PcLink {
  RecordingLink(super.hub, super.device, super.key, {required this.native});

  final bool native;
  final packets = <Map<String, dynamic>>[];

  @override
  bool get hasPointerPump => native;

  @override
  Future<bool> send(String json) async {
    sendFast(json);
    return true;
  }

  @override
  bool sendPointer(
    double dx,
    double dy,
    int buttons,
    int wheel, {
    bool immediate = false,
  }) {
    if (!native) return false;
    packets.add({'dx': dx, 'dy': dy, 'buttons': buttons, 'wheel': wheel});
    return true;
  }

  @override
  void sendFast(String json) =>
      packets.add(jsonDecode(json) as Map<String, dynamic>);
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  group('handshake', () {
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    late List<String> connects;
    late List<String> closed;
    late List<Map> sent;

    setUp(() {
      SharedPreferences.setMockInitialValues({});
      connects = [];
      closed = [];
      sent = [];
      messenger.setMockMethodCallHandler(
        const MethodChannel('agentpad/ws_events'),
        (_) async => null,
      );
      messenger.setMockMethodCallHandler(const MethodChannel('agentpad/ws'), (
        call,
      ) async {
        final args = call.arguments as Map;
        switch (call.method) {
          case 'connect':
            connects.add(args['id'] as String);
            return true;
          case 'send':
            sent.add(jsonDecode(args['text'] as String) as Map);
            return true;
          case 'close':
            closed.add(args['id'] as String);
        }
        return null;
      });
    });

    tearDown(() {
      messenger.setMockMethodCallHandler(
        const MethodChannel('agentpad/ws'),
        null,
      );
      messenger.setMockMethodCallHandler(
        const MethodChannel('agentpad/ws_events'),
        null,
      );
    });

    Future<void> settle() async {
      for (var i = 0; i < 20; i++) {
        await Future<void>.delayed(Duration.zero);
      }
    }

    Future<void> server(Map<String, Object?> msg) async {
      await messenger.handlePlatformMessage(
        'agentpad/ws_events',
        const StandardMethodCodec().encodeSuccessEnvelope({
          'id': connects.last,
          'event': 'text',
          'data': jsonEncode(msg),
        }),
        (_) {},
      );
      await settle();
    }

    Future<Hub> start(Device d) async {
      final hub = Hub(PadStore()..devices = [d]);
      addTearDown(hub.dispose);
      hub.sync();
      await settle();
      return hub;
    }

    Device paired() => Device(
      deviceId: 'pc-a',
      name: 'A',
      ips: ['10.0.0.5'],
      port: 9618,
      secret: 'k',
    );

    test(
      'paired device answers with HMAC and is online only after connected',
      () async {
        final d = paired();
        final hub = await start(d);
        expect(connects, hasLength(1));
        expect(sent, isEmpty);
        await server({'type': 'challenge', 'nonce': 'n1', 'device_id': 'pc-a'});
        expect(sent.single['type'], 'hello');
        expect(sent.single['auth'], authTag('k', 'n1'));
        expect(hub.online, isEmpty);
        expect(await hub.sendTo(d, '{}'), isFalse);
        await server({
          'type': 'connected',
          'device_id': 'pc-a',
          'os': 'windows',
        });
        expect(hub.online, {'pc-a'});
        expect(d.os, 'windows');
      },
    );

    test(
      'another PC on the same IP gets no credentials and changes nothing',
      () async {
        final d = paired();
        final hub = await start(d);
        await server({'type': 'challenge', 'nonce': 'n1', 'device_id': 'pc-b'});
        expect(sent, isEmpty);
        expect(closed, contains(connects.first));
        expect(hub.online, isEmpty);
        expect(d.deviceId, 'pc-a');
      },
    );

    test(
      'connected before the challenge or with another id is ignored',
      () async {
        final d = paired();
        final hub = await start(d);
        await server({'type': 'connected', 'device_id': 'pc-a'});
        expect(hub.online, isEmpty);
        final link = hub.links.values.single;
        link.onServerMessage(
          jsonEncode({'type': 'challenge', 'nonce': 'n', 'device_id': 'pc-a'}),
        );
        link.onServerMessage(
          jsonEncode({'type': 'connected', 'device_id': 'pc-b', 'os': 'x'}),
        );
        expect(hub.online, isEmpty);
        expect(d.deviceId, 'pc-a');
        expect(d.os, '');
      },
    );

    test('pairing code is sent and the returned secret is stored', () async {
      final d = Device(
        deviceId: '',
        name: 'New',
        ips: ['10.0.0.6'],
        port: 9618,
        pairCode: '0420',
      );
      final hub = await start(d);
      await server({'type': 'challenge', 'nonce': 'n1', 'device_id': 'pc-z'});
      expect(sent.single['type'], 'pair');
      expect(sent.single['code'], '0420');
      await server({'type': 'connected', 'device_id': 'pc-z', 'secret': 'S'});
      expect(d.secret, 'S');
      expect(d.pairCode, isEmpty);
      expect(d.deviceId, 'pc-z');
      expect(hub.online, hasLength(1));
    });

    test('rejected credentials stop reconnecting until re-paired', () async {
      final d = paired();
      final hub = await start(d);
      await server({'type': 'challenge', 'nonce': 'n1', 'device_id': 'pc-a'});
      await server({'type': 'auth_failed', 'reason': 'bad_auth'});
      expect(d.needsPairing, isTrue);
      expect(d.canAuthenticate, isFalse);
      expect(hub.online, isEmpty);
      expect(closed, contains(connects.single));
    });

    test('devices without a secret or code never connect', () async {
      await start(
        Device(deviceId: 'pc-a', name: 'A', ips: ['10.0.0.5'], port: 9618),
      );
      expect(connects, isEmpty);
    });

    test('sync replaces a stopped link', () async {
      final hub = await start(paired());
      expect(connects, hasLength(1));
      hub.links.values.single.stop();
      await settle();
      hub.sync();
      await settle();
      expect(connects, hasLength(2));
    });

    test('connected backfill keeps a single row per device id', () async {
      final saved = Device(
        deviceId: 'pc-z',
        name: 'Mac',
        ips: ['10.0.0.5'],
        port: 9618,
        secret: 'old',
      );
      final added = Device(
        deviceId: '',
        name: 'Mac',
        ips: ['10.0.0.6'],
        port: 9618,
        pairCode: '0420',
      );
      final store = PadStore()..devices = [saved, added];
      final hub = Hub(store);
      addTearDown(hub.dispose);
      hub.sync();
      await settle();
      final link = hub.links[Hub.keyOf(added)]!;
      link.onServerMessage(
        jsonEncode({'type': 'challenge', 'nonce': 'n1', 'device_id': 'pc-z'}),
      );
      link.onServerMessage(
        jsonEncode({
          'type': 'connected',
          'device_id': 'pc-z',
          'secret': 'S',
          'os': 'macos',
        }),
      );
      expect(store.devices, hasLength(1));
      expect(store.devices.single.deviceId, 'pc-z');
      expect(store.devices.single.ips, containsAll(['10.0.0.5', '10.0.0.6']));
      expect(hub.isOnline(store.devices.single), isTrue);
      expect(hub.links, hasLength(1));
    });

    test('hidden activity does not open a fallback socket', () async {
      final before = PcLink.fallbackSockets;
      messenger.setMockMethodCallHandler(const MethodChannel('agentpad/ws'), (
        call,
      ) async {
        final args = (call.arguments as Map?) ?? {};
        switch (call.method) {
          case 'connect':
            connects.add((args['id'] as String?) ?? '');
            return false;
          case 'wsVisible':
            return false;
          case 'close':
            closed.add((args['id'] as String?) ?? '');
        }
        return null;
      });
      final hub = await start(paired());
      expect(PcLink.fallbackSockets, before);
      expect(connects, isNotEmpty);
      hub.setActive(false);
    });

    test('a silent session is closed and retried', () async {
      PcLink.silenceLimit = const Duration(milliseconds: 40);
      addTearDown(() => PcLink.silenceLimit = const Duration(seconds: 10));
      final hub = await start(paired());
      await server({'type': 'challenge', 'nonce': 'n1', 'device_id': 'pc-a'});
      await server({'type': 'connected', 'device_id': 'pc-a'});
      expect(connects, hasLength(1));
      await Future<void>.delayed(const Duration(milliseconds: 250));
      expect(connects.length, greaterThan(1));
      hub.dispose();
    });
  });

  test(
    'switch releases native and fallback targets and all input uses one set',
    () async {
      for (final native in [true, false]) {
        final a = Device(deviceId: 'a', name: 'A', ips: ['a'], port: 9618);
        final b = Device(deviceId: 'b', name: 'B', ips: ['b'], port: 9618);
        final store = PadStore()..devices = [a, b];
        var cancelled = 0;
        final hub = Hub(store, onTargetsChanged: () => cancelled++);
        final first = RecordingLink(hub, a, 'a', native: native);
        final second = RecordingLink(hub, b, 'b', native: native);
        hub.links.addAll({'a': first, 'b': second});
        hub.online.addAll(['a', 'b']);
        hub.sendPointer(1, 1, 1, 0);
        store.selectDevice(b, true);
        expect(first.packets.last['buttons'], 0);
        expect(first.packets.last['dx'], 0);
        expect(cancelled, 1);
        final firstCount = first.packets.length;
        await hub.sendSelected(keyMsg('Enter', []));
        hub.sendPointer(1, 1, 0, 0);
        expect(first.packets, hasLength(firstCount));
        expect(second.packets, hasLength(2));
        expect(hub.online, {'a', 'b'});
        store.selectDevice(b, false);
        final secondCount = second.packets.length;
        await hub.sendSelected(
          textMsg('none', autoEnter: false, sendMode: 'text'),
        );
        hub.sendPointer(1, 1, 0, 0);
        expect(second.packets, hasLength(secondCount));
        hub.dispose();
      }
    },
  );

  test('throwing UI callbacks do not escape the hub', () {
    final hub = Hub(
      PadStore(),
      onChange: () => throw StateError('disposed sheet'),
    );
    expect(hub.sync, returnsNormally);
    expect(() => hub.setActive(false), returnsNormally);
  });

  test('paused hubs ignore sync and disposal clears online state', () {
    final store = PadStore()
      ..devices = [
        Device(deviceId: 'pc', name: 'PC', ips: ['127.0.0.1'], port: 9618),
      ];
    final hub = Hub(store, active: false);
    hub.sync();
    expect(hub.links, isEmpty);
    hub.links['pc'] = RecordingLink(
      hub,
      store.devices.single,
      'pc',
      native: true,
    );
    hub.online.add('pc');
    hub.dispose();
    hub.sync();
    expect(hub.links, isEmpty);
    expect(hub.online, isEmpty);
    expect(store.devices.single.selected, isTrue);
  });

  test(
    'mixed transports scale each online target once and retain wheel fractions',
    () {
      final store = PadStore()
        ..limitActiveDevices = false
        ..devices = [
          Device(
            deviceId: 'mac',
            name: 'Mac',
            ips: ['127.0.0.1'],
            port: 9618,
            os: 'macos',
          ),
          Device(
            deviceId: 'win',
            name: 'Windows',
            ips: ['127.0.0.2'],
            port: 9618,
            os: 'windows',
          ),
        ];
      final hub = Hub(store);
      final mac = RecordingLink(hub, store.devices[0], 'mac', native: true);
      final win = RecordingLink(hub, store.devices[1], 'win', native: false);
      hub.links.addAll({'mac': mac, 'win': win});
      hub.online.addAll(['mac', 'win']);

      hub.sendPointer(1, 2, 0, 1);
      expect(mac.packets.last['wheel'], 16);
      expect(win.packets.last['wheel'], 1);
      expect(mac.packets.last['dx'], 3);
      expect(win.packets.last['dy'], 6);

      for (var i = 0; i < 2; i++) {
        hub.sendPointer(0, 0, 0, 0.5);
        expect(mac.packets.last['wheel'], 8);
        expect(win.packets.last['wheel'], i);
      }
      expect(mac.packets.length, 3);
      expect(win.packets.length, 3);

      store.wheelReverseMac = true;
      hub.sendPointer(0, 0, 0, 0.25);
      expect(mac.packets.last['wheel'], -4);
      expect(win.packets.last['wheel'], 0);
      store.devices[1].selected = false;
      hub.sendPointer(0, 0, 0, 0.5);
      expect(win.packets.length, 4);
      store.devices[1].selected = true;
      hub.online.remove('win');
      hub.sendPointer(0, 0, 0, 0.5);
      expect(win.packets.length, 4);
      hub.online.add('win');
      hub.sendPointer(0, 0, 0, 0.75);
      expect(win.packets.last['wheel'], 1);

      hub.sendPointer(0, 0, 1, 0.75, immediate: true);
      win.stop();
      hub.online.add('win');
      hub.sendPointer(0, 0, 0, 0.25, immediate: true);
      expect(win.packets.last['wheel'], 0);
      expect(win.packets.reversed.take(2).map((p) => p['buttons']), [0, 1]);
    },
  );
}
