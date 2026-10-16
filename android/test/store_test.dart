import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'package:agentpad/hub.dart';
import 'package:agentpad/store.dart';

void main() {
  test('single target rules preserve zero and existing selection', () async {
    SharedPreferences.setMockInitialValues({});
    final store = PadStore();
    Device device(String id) =>
        Device(deviceId: id, name: id, ips: [id], port: 9618);
    store.upsert(device('a'));
    store.upsert(device('b'));
    expect(store.activeDevices.map((d) => d.deviceId), ['a']);
    store.selectDevice(store.devices.last, true);
    expect(store.activeDevices.map((d) => d.deviceId), ['b']);
    final stale = store.devices.first;
    store.upsert(device('a'));
    expect(store.activeDevices.map((d) => d.deviceId), ['b']);
    store.selectDevice(stale, true);
    expect(store.activeDevices.map((d) => d.deviceId), ['a']);
    store.selectDevice(store.devices.last, true);
    store.selectDevice(store.devices.last, false);
    expect(store.activeDevices, isEmpty);
    store.upsert(device('a'));
    expect(store.activeDevices, isEmpty);
    store.upsert(device('c'));
    expect(store.activeDevices.map((d) => d.deviceId), ['c']);
    store.limitActiveDevices = false;
    store.upsert(device('d'));
    expect(store.devices.last.selected, isFalse);
    store.selectDevice(store.devices.first, true);
    await store.save();
    final restored = PadStore();
    await restored.load();
    expect(restored.activeDevices.map((d) => d.deviceId), ['a', 'c']);
    restored.limitActiveDevices = true;
    expect(restored.activeDevices.map((d) => d.deviceId), ['a']);
    await restored.save();
    await restored.load();
    expect(restored.limitActiveDevices, isTrue);
    expect(restored.activeDevices.map((d) => d.deviceId), ['a']);
  });

  test(
    'legacy multiple selections load as first selected and zero stays zero',
    () async {
      for (final selected in [true, false]) {
        SharedPreferences.setMockInitialValues({
          'devices': jsonEncode([
            for (final id in ['a', 'b', 'c'])
              Device(
                deviceId: id,
                name: id,
                ips: [id],
                port: 9618,
                selected: selected && id != 'a',
              ).toJson(),
          ]),
        });
        final store = PadStore();
        await store.load();
        expect(store.limitActiveDevices, isTrue);
        expect(
          store.devices.where((d) => d.selected).map((d) => d.deviceId),
          selected ? ['b'] : <String>[],
        );
      }
    },
  );

  test('upsert merges by device_id and keeps others', () {
    final a = Device(deviceId: 'a', name: 'A', ips: ['1.1.1.1'], port: 9618);
    final b = Device(deviceId: 'b', name: 'B', ips: ['2.2.2.2'], port: 9618);
    var list = upsertDevice([], a);
    list = upsertDevice(list, b);
    list = upsertDevice(
      list,
      Device(
        deviceId: 'a',
        name: 'A2',
        ips: ['1.1.1.1', '3.3.3.3'],
        port: 9618,
      ),
    );
    expect(list.length, 2);
    expect(list[0].name, 'A'); // display name stays
    expect(list[0].ips, ['1.1.1.1', '3.3.3.3']);
    expect(list[1].deviceId, 'b');
  });

  test('same ip different device_id stays separate', () {
    final list = upsertDevice([
      Device(deviceId: 'a', name: 'A', ips: ['10.0.0.5'], port: 9618),
    ], Device(deviceId: 'b', name: 'B', ips: ['10.0.0.5'], port: 9618));
    expect(list.length, 2);
    expect(list[0].name, 'A');
    expect(list[1].name, 'B');
  });

  test('hub keeps different device ids separate when ips overlap', () {
    final hub = Hub(PadStore());
    final online = Device(
      deviceId: 'a',
      name: 'A',
      ips: ['10.0.0.5'],
      port: 9618,
    );
    hub.links['a'] = PcLink(hub, online, 'a');
    hub.online.add('a');

    final other = Device(
      deviceId: 'b',
      name: 'B',
      ips: ['10.0.0.5'],
      port: 9618,
    );
    expect(hub.isOnline(other), isFalse);
  });

  test('hub falls back to overlapping ip for a legacy device without id', () {
    final hub = Hub(PadStore());
    final online = Device(
      deviceId: 'a',
      name: 'A',
      ips: ['10.0.0.5'],
      port: 9618,
    );
    hub.links['a'] = PcLink(hub, online, 'a');
    hub.online.add('a');

    final legacy = Device(
      deviceId: '',
      name: 'legacy',
      ips: ['10.0.0.5'],
      port: 9618,
    );
    expect(hub.isOnline(legacy), isTrue);
  });

  test('upsert merges by overlapping ip when no device_id', () {
    final list = upsertDevice([
      Device(deviceId: '', name: 'old', ips: ['10.0.0.5'], port: 9618),
    ], Device(deviceId: 'x', name: 'new', ips: ['10.0.0.5'], port: 9618));
    expect(list.length, 1);
    expect(list.first.deviceId, 'x');
    expect(list.first.name, 'old');
  });

  test('device os persists and merges from newer device data', () {
    final list = upsertDevice(
      [
        Device(deviceId: 'a', name: 'A', ips: ['10.0.0.5'], port: 9618),
      ],
      Device(
        deviceId: 'a',
        name: 'A',
        ips: ['10.0.0.6'],
        port: 9618,
        os: 'windows',
      ),
    );
    expect(list.single.os, 'windows');
    final json = list.single.toJson();
    expect(Device.fromJson(json).os, 'windows');
  });

  test('scan secret clears a stale pair code on the same ip', () {
    var list = upsertDevice(
      [],
      Device(
        deviceId: '',
        name: 'manual',
        ips: ['10.0.0.8'],
        port: 9618,
        pairCode: '0420',
      ),
    );
    list = upsertDevice(
      list,
      Device(
        deviceId: '',
        name: 'scan',
        ips: ['10.0.0.8'],
        port: 9618,
        secret: 'new-secret',
        pairCode: '',
      ),
    );
    expect(list.length, 1);
    expect(list.single.pairCode, isEmpty);
    expect(list.single.secret, 'new-secret');
  });

  test('upsert collapses rows that already share a device id', () {
    final list = upsertDevice(
      [
        Device(
          deviceId: 'a',
          name: 'A',
          ips: ['1.1.1.1'],
          port: 9618,
          secret: 's',
        ),
        Device(deviceId: 'a', name: 'A2', ips: ['2.2.2.2'], port: 9618),
      ],
      Device(
        deviceId: 'a',
        name: 'A',
        ips: ['1.1.1.1'],
        port: 9618,
        secret: 's',
      ),
    );
    expect(list, hasLength(1));
    expect(list.single.deviceId, 'a');
    expect(list.single.name, 'A');
    expect(list.single.ips, ['1.1.1.1', '2.2.2.2']);
  });

  test('collect all ips setting defaults off and persists', () async {
    SharedPreferences.setMockInitialValues({});
    final s = PadStore();
    await s.load();
    expect(s.collectAllIps, isFalse);
    s.collectAllIps = true;
    await s.save();
    final s2 = PadStore();
    await s2.load();
    expect(s2.collectAllIps, isTrue);
  });

  test('send clears input only after a successful send', () {
    expect(sendClearsInput(true), isTrue);
    expect(sendClearsInput(false), isFalse);
  });

  test('shortcuts persist roundtrip', () async {
    SharedPreferences.setMockInitialValues({});
    final s = PadStore();
    await s.load();
    s.shortcuts.add(Shortcut(id: 'x', label: 'Tab', key: 'Tab', modifiers: []));
    s.autoEnter = true;
    await s.save();
    final s2 = PadStore();
    await s2.load();
    expect(s2.autoEnter, isTrue);
    expect(s2.shortcuts.any((k) => k.label == 'Tab'), isTrue);
    expect(s2.shortcuts.any((k) => k.label == 'Esc'), isTrue);
  });

  test('theme persists', () async {
    SharedPreferences.setMockInitialValues({});
    final s = PadStore();
    await s.load();
    expect(s.theme, 'system');
    s.theme = 'dark';
    await s.save();
    final s2 = PadStore();
    await s2.load();
    expect(s2.theme, 'dark');
  });

  test('theme color defaults to blue, validates, and persists', () async {
    SharedPreferences.setMockInitialValues({});
    final s = PadStore();
    await s.load();
    expect(s.themeColor, 'blue');
    s.themeColor = 'green';
    await s.save();
    final s2 = PadStore();
    await s2.load();
    expect(s2.themeColor, 'green');

    SharedPreferences.setMockInitialValues({'theme_color': 'purple'});
    final invalid = PadStore();
    await invalid.load();
    expect(invalid.themeColor, 'blue');
  });

  test('pointer mode and wheel settings persist', () async {
    SharedPreferences.setMockInitialValues({});
    final s = PadStore();
    await s.load();
    expect(s.pointerMode, 'trackpad');
    expect(s.homePointerQuickSwitch, isTrue);
    expect(s.deviceStripPlacement, 'input');
    expect(s.wheelSide, 'right');
    expect(s.wheelReverseWindows, isFalse);
    expect(s.wheelReverseMac, isFalse);
    s.pointerMode = 'trackball';
    s.homePointerQuickSwitch = false;
    s.deviceStripPlacement = 'top';
    s.wheelSide = 'left';
    s.wheelReverseWindows = true;
    s.wheelReverseMac = true;
    await s.save();
    final s2 = PadStore();
    await s2.load();
    expect(s2.pointerMode, 'trackball');
    expect(s2.homePointerQuickSwitch, isFalse);
    expect(s2.deviceStripPlacement, 'top');
    expect(s2.wheelSide, 'left');
    expect(s2.wheelReverseWindows, isTrue);
    expect(s2.wheelReverseMac, isTrue);
  });

  test('pointer hz defaults to 60 and persists', () async {
    SharedPreferences.setMockInitialValues({});
    final s = PadStore();
    await s.load();
    expect(s.pointerHz, 60);
    expect(s.pointerHzManual, isFalse);
    s.pointerHz = 240;
    s.pointerHzManual = true;
    await s.save();
    final s2 = PadStore();
    await s2.load();
    expect(s2.pointerHz, 240);
    expect(s2.pointerHzManual, isTrue);

    SharedPreferences.setMockInitialValues({'pointer_hz': 90});
    final invalid = PadStore();
    await invalid.load();
    expect(invalid.pointerHz, 60);
  });

  test('pointer and platform wheel speed default and persist', () async {
    SharedPreferences.setMockInitialValues({});
    final s = PadStore();
    await s.load();
    expect(s.pointerSpeedWindows, 3);
    expect(s.pointerSpeedMac, 3);
    expect(s.wheelSpeedWindows, 1);
    expect(s.wheelSpeedMac, 16);
    expect(s.wheelSpeedFor('windows'), 1);
    expect(s.wheelSpeedFor('macos'), 16);
    s.pointerSpeedWindows = 5;
    s.pointerSpeedMac = 7;
    s.wheelSpeedWindows = 6;
    s.wheelSpeedMac = 28;
    await s.save();
    final s2 = PadStore();
    await s2.load();
    expect(s2.pointerSpeedWindows, 5);
    expect(s2.pointerSpeedMac, 7);
    expect(s2.wheelSpeedWindows, 6);
    expect(s2.wheelSpeedMac, 28);

    SharedPreferences.setMockInitialValues({
      'pointer_speed_windows': 8.0,
      'pointer_speed_mac': 6.0,
      'wheel_factor_windows': 11.0,
      'wheel_factor_mac': 25.0,
    });
    final invalid = PadStore();
    await invalid.load();
    expect(invalid.pointerSpeedWindows, 3);
    expect(invalid.pointerSpeedMac, 6);
    expect(invalid.wheelSpeedWindows, 7);
    expect(invalid.wheelSpeedMac, 24);

    SharedPreferences.setMockInitialValues({
      'pointer_speed': 4.0,
      'wheel_speed': 4.0,
    });
    final legacy = PadStore();
    await legacy.load();
    expect(legacy.pointerSpeedWindows, 4);
    expect(legacy.pointerSpeedMac, 4);
    expect(legacy.wheelSpeedWindows, 1);
    expect(legacy.wheelSpeedMac, 16);
  });

  test('non-manual 240 falls back before auto detect', () async {
    SharedPreferences.setMockInitialValues({
      'pointer_hz': 240,
      'pointer_hz_manual': false,
    });
    final s = PadStore();
    await s.load();
    expect(s.pointerHzManual, isFalse);
    expect(s.pointerHz, 60);
  });

  test('pointer size persists', () async {
    SharedPreferences.setMockInitialValues({});
    final s = PadStore();
    await s.load();
    expect(s.pointerSize, 'medium');
    s.pointerSize = 'large';
    await s.save();
    final s2 = PadStore();
    await s2.load();
    expect(s2.pointerSize, 'large');
  });

  test('input height defaults to medium and persists', () async {
    SharedPreferences.setMockInitialValues({});
    final s = PadStore();
    await s.load();
    expect(s.inputHeight, 'medium');
    s.inputHeight = 'huge';
    await s.save();
    final s2 = PadStore();
    await s2.load();
    expect(s2.inputHeight, 'huge');

    SharedPreferences.setMockInitialValues({'input_height': 'short'});
    final legacy = PadStore();
    await legacy.load();
    expect(legacy.inputHeight, 'medium');

    SharedPreferences.setMockInitialValues({'input_height': 'huge'});
    final ok = PadStore();
    await ok.load();
    expect(ok.inputHeight, 'huge');
  });

  test('landscape pointer side defaults, validates, and persists', () async {
    SharedPreferences.setMockInitialValues({});
    final s = PadStore();
    await s.load();
    expect(s.landscapePointerSide, 'right');
    expect(s.forceLandscape, isFalse);
    s.landscapePointerSide = 'left';
    s.forceLandscape = true;
    await s.save();
    final s2 = PadStore();
    await s2.load();
    expect(s2.landscapePointerSide, 'left');
    expect(s2.forceLandscape, isTrue);

    SharedPreferences.setMockInitialValues({
      'landscape_pointer_side': 'bottom',
    });
    final invalid = PadStore();
    await invalid.load();
    expect(invalid.landscapePointerSide, 'right');
  });

  test('long press haptic defaults on and persists', () async {
    SharedPreferences.setMockInitialValues({});
    final s = PadStore();
    await s.load();
    expect(s.longPressHaptic, isTrue);
    s.longPressHaptic = false;
    await s.save();
    final s2 = PadStore();
    await s2.load();
    expect(s2.longPressHaptic, isFalse);
  });

  test(
    'voice auto-send delay defaults to half a second and persists',
    () async {
      SharedPreferences.setMockInitialValues({});
      final s = PadStore();
      await s.load();
      expect(s.voiceDelayMs, 500);
      expect(s.voiceDelay, const Duration(milliseconds: 500));
      s.voiceDelayMs = 1000;
      await s.save();
      final s2 = PadStore();
      await s2.load();
      expect(s2.voiceDelayMs, 1000);
      expect(s2.voiceDelay, const Duration(seconds: 1));
    },
  );

  test('invalid voice auto-send delay falls back to half a second', () async {
    SharedPreferences.setMockInitialValues({'voice_delay_ms': 750});
    final s = PadStore();
    await s.load();
    expect(s.voiceDelayMs, 500);
  });

  test('legacy trackpoint preference migrates to pointer mode', () async {
    SharedPreferences.setMockInitialValues({'trackpoint': true});
    final s = PadStore();
    await s.load();
    expect(s.pointerMode, 'trackpoint');
  });
}
