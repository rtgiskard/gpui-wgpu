use crate::{
    AnyElement, App, Bounds, Element, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    Pixels, Window,
};

/// Builds a `Deferred` element, which delays the layout and paint of its child.
pub fn deferred(child: impl IntoElement) -> Deferred {
    Deferred {
        child: Some(child.into_any_element()),
        priority: 0,
    }
}

/// An element which delays the painting of its child until after all of
/// its ancestors, while keeping its layout as part of the current element tree.
pub struct Deferred {
    child: Option<AnyElement>,
    priority: usize,
}

impl Deferred {
    /// Sets the `priority` value of the `deferred` element, which
    /// determines the drawing order relative to other deferred elements,
    /// with higher values being drawn on top.
    pub fn with_priority(mut self, priority: usize) -> Self {
        self.priority = priority;
        self
    }
}

impl Element for Deferred {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<crate::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let layout_id = self.child.as_mut().unwrap().request_layout(window, cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let child = self.child.take().unwrap();
        let element_offset = window.element_offset();
        window.defer_draw(child, element_offset, self.priority, None)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }
}

impl IntoElement for Deferred {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Deferred {
    /// Sets a priority for the element. A higher priority conceptually means painting the element
    /// on top of deferred draws with a lower priority (i.e. closer to the viewer).
    pub fn priority(mut self, priority: usize) -> Self {
        self.priority = priority;
        self
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    use crate::{
        App, Context, Entity, StyleRefinement, TestAppContext, Window, WindowOptions, anchored,
        deferred, div, point, prelude::*, px, size,
    };

    struct SubscriptionLeaf {
        value: usize,
        output: Rc<Cell<usize>>,
    }

    struct SubscriptionPanel {
        leaf: Entity<SubscriptionLeaf>,
        deferred_child: bool,
        renders: Rc<Cell<usize>>,
    }

    impl Render for SubscriptionPanel {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.renders.set(self.renders.get() + 1);
            let leaf = self.leaf.clone();
            let child = crate::canvas(
                |_, _, _| {},
                move |_, _, _, cx| {
                    let leaf = leaf.read(cx);
                    leaf.output.set(leaf.value);
                },
            )
            .size(px(50.));
            if self.deferred_child {
                deferred(child).into_any_element()
            } else {
                child.into_any_element()
            }
        }
    }

    struct SubscriptionRoot {
        panel: Entity<SubscriptionPanel>,
        deferred_panel: bool,
        renders: Rc<Cell<usize>>,
    }

    impl Render for SubscriptionRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.renders.set(self.renders.get() + 1);
            let panel = self
                .panel
                .clone()
                .cached(StyleRefinement::default().size(px(100.)));
            if self.deferred_panel {
                deferred(panel).into_any_element()
            } else {
                panel.into_any_element()
            }
        }
    }

    #[gpui::test]
    fn test_cached_reuse_keeps_entity_subscriptions(cx: &mut TestAppContext) {
        for deferred_child in [true, false] {
            for deferred_panel in [false, true] {
                let output = Rc::new(Cell::new(0));
                let renders = Rc::new(Cell::new(0));
                let root_renders = Rc::new(Cell::new(0));
                let leaf = cx.new(|_| SubscriptionLeaf {
                    value: 1,
                    output: output.clone(),
                });
                let panel = cx.new(|_| SubscriptionPanel {
                    leaf: leaf.clone(),
                    deferred_child,
                    renders: renders.clone(),
                });
                let window = cx.open_window(size(px(800.), px(600.)), |_, _| SubscriptionRoot {
                    panel,
                    deferred_panel,
                    renders: root_renders.clone(),
                });
                cx.run_until_parked();
                assert_eq!(output.get(), 1);
                let initial_renders = renders.get();
                for _ in 0..2 {
                    window
                        .update(cx, |_, _, cx| cx.notify())
                        .expect("notify root");
                    cx.run_until_parked();
                    assert_eq!(
                        renders.get(),
                        initial_renders,
                        "panel should reuse its cache"
                    );
                }

                let renders_before_notify = root_renders.get();
                leaf.update(cx, |leaf, cx| {
                    leaf.value = 2;
                    cx.notify();
                });
                cx.run_until_parked();
                let context =
                    format!("deferred child {deferred_child}, deferred panel {deferred_panel}");
                assert!(
                    root_renders.get() > renders_before_notify,
                    "leaf notification should redraw: {context}"
                );
                // A plain entity read has no dispatch-tree path to invalidate the
                // cached panel, so refresh to run the paint callback again.
                window
                    .update(cx, |_, window, _| window.refresh())
                    .expect("refresh canvas");
                cx.run_until_parked();
                assert_eq!(output.get(), 2, "{context}");
            }
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum OpenWindowPhase {
        Render,
        Prepaint,
        Paint,
    }

    struct NestedWindowState {
        phase: OpenWindowPhase,
        opened: Cell<bool>,
        outer_model: Entity<usize>,
        inner_model: Entity<usize>,
        inner_renders: Rc<Cell<usize>>,
        inner_view: RefCell<Option<Entity<NestedNotificationView>>>,
    }

    impl NestedWindowState {
        fn open(&self, phase: OpenWindowPhase, cx: &mut App) {
            if self.phase != phase {
                return;
            }
            // This read must survive the inner draw, both in the frame and
            // in the cached/deferred scope that will be restored on reuse.
            self.outer_model.read(cx);
            if self.opened.replace(true) {
                return;
            }
            let model = self.inner_model.clone();
            let renders = self.inner_renders.clone();
            cx.open_window(WindowOptions::default(), |_, cx| {
                let view = cx.new(|_| NestedNotificationView { model, renders });
                self.inner_view.replace(Some(view.clone()));
                view
            })
            .expect("open nested window");
        }
    }

    struct NestedNotificationView {
        model: Entity<usize>,
        renders: Rc<Cell<usize>>,
    }

    impl Render for NestedNotificationView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.model.read(cx);
            self.renders.set(self.renders.get() + 1);
            div().size_full()
        }
    }

    struct NestedWindowPanel {
        state: Rc<NestedWindowState>,
        deferred_child: bool,
        renders: Rc<Cell<usize>>,
    }

    impl Render for NestedWindowPanel {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.renders.set(self.renders.get() + 1);
            self.state.open(OpenWindowPhase::Render, cx);
            let prepaint_state = self.state.clone();
            let paint_state = self.state.clone();
            let child = crate::canvas(
                move |_, _, cx| prepaint_state.open(OpenWindowPhase::Prepaint, cx),
                move |_, _, _, cx| paint_state.open(OpenWindowPhase::Paint, cx),
            )
            .size(px(50.));
            if self.deferred_child {
                deferred(child).into_any_element()
            } else {
                child.into_any_element()
            }
        }
    }

    struct NestedWindowRoot {
        model: Entity<usize>,
        panel: Entity<NestedWindowPanel>,
        deferred_panel: bool,
        renders: Rc<Cell<usize>>,
    }

    impl Render for NestedWindowRoot {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            // This read is outside the cached panel's scope and must not be
            // replaced by the inner window's frame reads either.
            self.model.read(cx);
            self.renders.set(self.renders.get() + 1);
            let panel = self
                .panel
                .clone()
                .cached(StyleRefinement::default().size(px(100.)));
            if self.deferred_panel {
                deferred(panel).into_any_element()
            } else {
                panel.into_any_element()
            }
        }
    }

    #[gpui::test]
    fn test_nested_window_draw_keeps_independent_subscriptions(cx: &mut TestAppContext) {
        for phase in [
            OpenWindowPhase::Render,
            OpenWindowPhase::Prepaint,
            OpenWindowPhase::Paint,
        ] {
            for deferred_child in [false, true] {
                for deferred_panel in [false, true] {
                    let root_model = cx.new(|_| 0usize);
                    let outer_model = cx.new(|_| 0usize);
                    let inner_model = cx.new(|_| 0usize);
                    let outer_renders = Rc::new(Cell::new(0));
                    let inner_renders = Rc::new(Cell::new(0));
                    let panel_renders = Rc::new(Cell::new(0));
                    let state = Rc::new(NestedWindowState {
                        phase,
                        opened: Cell::new(false),
                        outer_model: outer_model.clone(),
                        inner_model: inner_model.clone(),
                        inner_renders: inner_renders.clone(),
                        inner_view: RefCell::new(None),
                    });
                    let panel = cx.new(|_| NestedWindowPanel {
                        state: state.clone(),
                        deferred_child,
                        renders: panel_renders.clone(),
                    });
                    let window =
                        cx.open_window(size(px(800.), px(600.)), |_, _| NestedWindowRoot {
                            model: root_model.clone(),
                            panel,
                            deferred_panel,
                            renders: outer_renders.clone(),
                        });
                    cx.run_until_parked();
                    assert!(state.opened.get());
                    assert!(inner_renders.get() > 0);
                    let initial_panel_renders = panel_renders.get();
                    let context = format!(
                        "{phase:?}, deferred child {deferred_child}, deferred panel {deferred_panel}"
                    );

                    for _ in 0..2 {
                        window.update(cx, |_, _, cx| cx.notify()).unwrap();
                        cx.run_until_parked();
                        assert_eq!(panel_renders.get(), initial_panel_renders, "{context}");
                    }

                    for model in [&root_model, &outer_model] {
                        let outer_before = outer_renders.get();
                        let inner_before = inner_renders.get();
                        model.update(cx, |_, cx| cx.notify());
                        cx.run_until_parked();
                        assert!(outer_renders.get() > outer_before, "{context}");
                        assert_eq!(inner_renders.get(), inner_before, "{context}");
                        assert_eq!(panel_renders.get(), initial_panel_renders, "{context}");
                    }

                    let outer_before = outer_renders.get();
                    let inner_before = inner_renders.get();
                    inner_model.update(cx, |_, cx| cx.notify());
                    cx.run_until_parked();
                    assert!(inner_renders.get() > inner_before, "{context}");
                    assert_eq!(outer_renders.get(), outer_before, "{context}");

                    // The inner root was allocated before its first draw;
                    // construction reads must not subscribe the outer window.
                    let outer_before = outer_renders.get();
                    let inner_before = inner_renders.get();
                    state
                        .inner_view
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .update(cx, |_, cx| cx.notify());
                    cx.run_until_parked();
                    assert!(inner_renders.get() > inner_before, "{context}");
                    assert_eq!(outer_renders.get(), outer_before, "{context}");
                }
            }
        }
    }

    /// A stand-in for a dock panel hosting a popover (deferred draw) whose
    /// content opens another popover (a deferred draw created while
    /// prepainting the first one's content).
    struct PanelView;

    impl Render for PanelView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().key_context("Panel").size_full().child(
                deferred(
                    anchored().position(point(px(10.), px(10.))).child(
                        div().key_context("Popover").w(px(200.)).h(px(200.)).child(
                            deferred(
                                anchored().position(point(px(30.), px(30.))).child(
                                    div()
                                        .key_context("NestedMenu")
                                        .debug_selector(|| "NESTED_MENU".into())
                                        .w(px(50.))
                                        .h(px(50.)),
                                ),
                            )
                            .with_priority(2),
                        ),
                    ),
                )
                .with_priority(1),
            )
        }
    }

    struct RootView {
        panel: Entity<PanelView>,
    }

    impl Render for RootView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().key_context("Root").size_full().child(
                self.panel
                    .clone()
                    .cached(StyleRefinement::default().size_full()),
            )
        }
    }

    /// Regression test for a crash with nested deferred draws (e.g. a popover
    /// menu inside a popover hosted by a cached dock panel). Prepaint indices
    /// recorded during the deferred draw rounds must index the same
    /// `deferred_draws` vector that `reuse_prepaint` slices on the next frame;
    /// previously they were measured against a transient per-round vector, so
    /// reusing the panel's subtree grafted the wrong deferred draws and
    /// panicked in the dispatch tree.
    #[gpui::test]
    fn test_nested_deferred_draws_with_reused_views(cx: &mut TestAppContext) {
        let window = cx.open_window(size(px(800.), px(600.)), |_, cx| {
            let panel = cx.new(|_| PanelView);
            RootView { panel }
        });
        cx.run_until_parked();

        let menu_bounds = window
            .update(cx, |_, window, _| {
                window
                    .rendered_frame
                    .debug_bounds
                    .get("NESTED_MENU")
                    .copied()
            })
            .unwrap()
            .expect("NESTED_MENU debug bounds not found");
        assert_eq!(menu_bounds.size, size(px(50.), px(50.)));

        // Re-render only the root view; the panel is cached, so its subtree -
        // including both deferred draw records - is reused from the previous
        // frame.
        window.update(cx, |_, _, cx| cx.notify()).unwrap();
        cx.run_until_parked();

        // Reuse the subtree a second time, exercising ranges that were
        // themselves recorded during a reused frame.
        window.update(cx, |_, _, cx| cx.notify()).unwrap();
        cx.run_until_parked();

        // Re-render the panel itself again to prove the popovers still draw.
        window
            .update(cx, |root, _, cx| {
                root.panel.update(cx, |_, cx| cx.notify());
            })
            .unwrap();
        cx.run_until_parked();

        window
            .update(cx, |_, window, _| {
                assert_eq!(window.rendered_frame.deferred_draws.len(), 2);
                assert!(
                    window
                        .rendered_frame
                        .debug_bounds
                        .contains_key("NESTED_MENU")
                );
            })
            .unwrap();
    }
}
