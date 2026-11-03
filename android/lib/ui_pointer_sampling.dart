import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';

class UiPointerSampling extends StatefulWidget {
  const UiPointerSampling({
    super.key,
    required this.refreshRate,
    required this.child,
  });

  final Future<double?> Function() refreshRate;
  final Widget child;

  @override
  State<UiPointerSampling> createState() => _UiPointerSamplingState();
}

class _UiPointerSamplingState extends State<UiPointerSampling> {
  static final _active = <_UiPointerSamplingState>[];
  static bool _previousEnabled = false;
  static Duration _previousOffset = Duration.zero;
  double _nativeRate = 0;
  Duration _offset = Duration.zero;

  @override
  void initState() {
    super.initState();
    unawaited(_readRefreshRate());
  }

  Future<void> _readRefreshRate() async {
    try {
      final rate = await widget.refreshRate();
      if (!mounted || rate == null || !rate.isFinite || rate <= 0) return;
      _nativeRate = rate;
      _updateSampling();
    } catch (_) {}
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _updateSampling();
  }

  void _updateSampling() {
    final displayRate = View.of(context).display.refreshRate;
    final rate = _nativeRate > displayRate ? _nativeRate : displayRate;
    final enabled =
        !kIsWeb &&
        defaultTargetPlatform == TargetPlatform.android &&
        rate.isFinite &&
        rate >= 90;
    final binding = GestureBinding.instance;
    if (enabled) {
      _offset = Duration(
        microseconds: -(1000000 / rate).round().clamp(1000, 6000),
      );
      if (!_active.contains(this)) {
        if (_active.isEmpty) {
          _previousEnabled = binding.resamplingEnabled;
          _previousOffset = binding.samplingOffset;
        }
        _active.add(this);
      }
    } else {
      if (!_active.remove(this)) return;
    }
    _applySampling();
  }

  static void _applySampling() {
    final binding = GestureBinding.instance;
    binding.resamplingEnabled = _active.isNotEmpty || _previousEnabled;
    binding.samplingOffset = _active.isEmpty
        ? _previousOffset
        : _active.last._offset;
  }

  @override
  void dispose() {
    if (_active.remove(this)) _applySampling();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => widget.child;
}
