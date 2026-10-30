const pointerHzGears = [60, 120, 240, 320, 480, 560, 960];

int pointerHzForDisplay(double? refreshRate) {
  if (refreshRate == null || !refreshRate.isFinite || refreshRate <= 0) {
    return 60;
  }
  final target = refreshRate * 2;
  return pointerHzGears.lastWhere((hz) => hz <= target, orElse: () => 60);
}

class PointerCoalescer {
  double dx = 0;
  double dy = 0;
  int buttons = 0;
  double wheel = 0;
  bool pending = false;

  void add(double ddx, double ddy, int btn, double wh) {
    dx += ddx;
    dy += ddy;
    buttons = btn;
    wheel += wh;
    pending = true;
  }

  Map<String, num>? tick() {
    if (!pending) return null;
    pending = false;
    final out = {'dx': dx, 'dy': dy, 'buttons': buttons, 'wheel': wheel};
    dx = 0;
    dy = 0;
    wheel = 0;
    return out;
  }
}

/// Fixed 16ms slots from the first sample. Later samples do not push the slot.
class PointerCadence {
  PointerCadence({this.period = const Duration(milliseconds: 16)});

  Duration period;
  Duration? _next;

  bool due(Duration now) {
    final next = _next;
    if (next == null) {
      _next = now + period;
      return false;
    }
    if (now < next) return false;
    var slot = next;
    do {
      slot += period;
    } while (slot <= now);
    _next = slot;
    return true;
  }

  void markSent(Duration now) {
    _next = now + period;
  }

  void reset() {
    _next = null;
  }
}
