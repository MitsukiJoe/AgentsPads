import 'dart:math' as math;

import 'package:flutter/rendering.dart';
import 'package:flutter/widgets.dart';

class AdaptiveHeightColumn extends MultiChildRenderObjectWidget {
  const AdaptiveHeightColumn({
    super.key,
    required this.fillIndex,
    required this.minimumFillHeight,
    required super.children,
  }) : assert(fillIndex >= 0 && fillIndex < children.length),
       assert(minimumFillHeight >= 0);

  final int fillIndex;
  final double minimumFillHeight;

  @override
  RenderObject createRenderObject(BuildContext context) =>
      RenderAdaptiveHeightColumn(fillIndex, minimumFillHeight);

  @override
  void updateRenderObject(
    BuildContext context,
    RenderAdaptiveHeightColumn renderObject,
  ) {
    renderObject
      ..fillIndex = fillIndex
      ..minimumFillHeight = minimumFillHeight;
  }
}

class _AdaptiveParentData extends ContainerBoxParentData<RenderBox> {}

class RenderAdaptiveHeightColumn extends RenderBox
    with
        ContainerRenderObjectMixin<
          RenderBox,
          ContainerBoxParentData<RenderBox>
        >,
        RenderBoxContainerDefaultsMixin<
          RenderBox,
          ContainerBoxParentData<RenderBox>
        > {
  RenderAdaptiveHeightColumn(this._fillIndex, this._minimumFillHeight);

  int _fillIndex;
  set fillIndex(int value) {
    if (_fillIndex == value) return;
    _fillIndex = value;
    markNeedsLayout();
  }

  double _minimumFillHeight;
  set minimumFillHeight(double value) {
    if (_minimumFillHeight == value) return;
    _minimumFillHeight = value;
    markNeedsLayout();
  }

  @override
  void setupParentData(RenderBox child) {
    if (child.parentData is! ContainerBoxParentData<RenderBox>) {
      child.parentData = _AdaptiveParentData();
    }
  }

  Size _measure(
    BoxConstraints constraints,
    Size Function(RenderBox, BoxConstraints) layout,
  ) {
    assert(constraints.hasBoundedWidth);
    final loose = BoxConstraints(maxWidth: constraints.maxWidth);
    var fixedHeight = 0.0;
    RenderBox? fill;
    var index = 0;
    for (var child = firstChild; child != null; child = childAfter(child)) {
      if (index++ == _fillIndex) {
        fill = child;
      } else {
        fixedHeight += layout(child, loose).height;
      }
    }
    final fillHeight = math.max(
      _minimumFillHeight,
      constraints.minHeight - fixedHeight,
    );
    layout(fill!, loose.copyWith(minHeight: fillHeight, maxHeight: fillHeight));
    return constraints.constrain(
      Size(constraints.maxWidth, fixedHeight + fillHeight),
    );
  }

  @override
  Size computeDryLayout(BoxConstraints constraints) =>
      _measure(constraints, (child, box) => child.getDryLayout(box));

  @override
  void performLayout() {
    size = _measure(constraints, (child, box) {
      child.layout(box, parentUsesSize: true);
      return child.size;
    });
    var top = 0.0;
    for (var child = firstChild; child != null; child = childAfter(child)) {
      final data = child.parentData! as ContainerBoxParentData<RenderBox>;
      data.offset = Offset((size.width - child.size.width) / 2, top);
      top += child.size.height;
    }
  }

  @override
  void paint(PaintingContext context, Offset offset) =>
      defaultPaint(context, offset);

  @override
  bool hitTestChildren(BoxHitTestResult result, {required Offset position}) =>
      defaultHitTestChildren(result, position: position);
}
