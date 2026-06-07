use std::{
    cmp,
    sync::{Arc, atomic::AtomicUsize},
};

use gpui::{
    Action, AnyElement, AnyEntity, App, Axis, Bounds, Context, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, Pixels, Render, SharedString, TaskExt, WeakEntity, Window,
};
use project::Project;
use terminal::Terminal;
use terminal_view::TerminalView;
use ui::prelude::*;
use workspace::{
    ActivePaneDecorator, Pane, PaneGroup, SplitDirection, SplitMode, Workspace, WorkspaceId,
    item::{Item, ItemEvent, TabContentParams, TabTooltipContent},
    pane,
};

pub struct TerminalTab {
    workspace: WeakEntity<Workspace>,
    workspace_id: Option<WorkspaceId>,
    project: Entity<Project>,
    center: PaneGroup,
    active_pane: Entity<Pane>,
    focus_handle: FocusHandle,
    pane_history_timestamp: Arc<AtomicUsize>,
}

impl TerminalTab {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        workspace_id: Option<WorkspaceId>,
        project: Entity<Project>,
        terminal: Entity<Terminal>,
        custom_title: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let pane_history_timestamp = Arc::new(AtomicUsize::new(0));
        let active_pane = new_terminal_tab_pane(
            workspace.clone(),
            project.clone(),
            pane_history_timestamp.clone(),
            window,
            cx,
        );
        let mut center = PaneGroup::new(active_pane.clone());
        center.set_is_center(true);
        center.mark_positions(cx);

        let mut tab = Self {
            workspace,
            workspace_id,
            project,
            center,
            active_pane,
            focus_handle: focus_handle.clone(),
            pane_history_timestamp,
        };
        tab.subscribe_to_pane(&tab.active_pane.clone(), window, cx);
        cx.on_focus_in(&focus_handle, window, |tab, window, cx| {
            tab.focus_active_pane(window, cx);
        })
        .detach();
        tab.add_terminal_to_pane(
            tab.active_pane.clone(),
            terminal,
            custom_title,
            true,
            window,
            cx,
        );
        tab
    }

    pub fn active_terminal_view(&self, cx: &App) -> Option<Entity<TerminalView>> {
        self.active_pane
            .read(cx)
            .active_item()
            .and_then(|item| item.downcast::<TerminalView>())
    }

    pub fn active_terminal(&self, cx: &App) -> Option<Entity<Terminal>> {
        self.active_terminal_view(cx)
            .map(|view| view.read(cx).terminal().clone())
    }

    pub fn active_terminal_custom_title(&self, cx: &App) -> Option<String> {
        self.active_terminal_view(cx)
            .and_then(|view| view.read(cx).custom_title().map(str::to_owned))
    }

    pub fn active_pane_size(&self, _cx: &App) -> Option<gpui::Size<Pixels>> {
        self.center
            .bounding_box_for_pane(&self.active_pane)
            .map(|bounds| bounds.size)
    }

    pub fn split_terminal(
        &mut self,
        terminal: Entity<Terminal>,
        custom_title: Option<String>,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old_pane = self.active_pane.clone();
        let new_pane = new_terminal_tab_pane(
            self.workspace.clone(),
            self.project.clone(),
            self.pane_history_timestamp.clone(),
            window,
            cx,
        );
        self.subscribe_to_pane(&new_pane, window, cx);
        self.add_terminal_to_pane(new_pane.clone(), terminal, custom_title, true, window, cx);
        self.center.split(&old_pane, &new_pane, direction, cx);
        self.focus_pane(new_pane, window, cx);
        cx.emit(ItemEvent::UpdateTab);
        cx.notify();
    }

    pub fn resize_active_pane(
        &mut self,
        axis: Axis,
        amount: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bounds = Bounds::new(Default::default(), window.viewport_size());
        self.center
            .resize(&self.active_pane, axis, amount, &bounds, cx);
        cx.notify();
    }

    pub fn reset_pane_sizes(&mut self, cx: &mut Context<Self>) {
        self.center.reset_pane_sizes(cx);
        cx.notify();
    }

    pub fn close_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active_pane = self.active_pane.clone();
        active_pane.update(cx, |pane, cx| {
            pane.close_active_item(
                &pane::CloseActiveItem {
                    save_intent: None,
                    close_pinned: false,
                },
                window,
                cx,
            )
            .detach_and_log_err(cx);
        });
    }

    fn add_terminal_to_pane(
        &mut self,
        pane: Entity<Pane>,
        terminal: Entity<Terminal>,
        custom_title: Option<String>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let terminal_view = cx.new(|cx| {
            TerminalView::new_with_custom_title(
                terminal,
                self.workspace.clone(),
                self.workspace_id,
                self.project.downgrade(),
                custom_title,
                window,
                cx,
            )
        });
        pane.update(cx, |pane, cx| {
            pane.add_item(Box::new(terminal_view), true, focus, None, window, cx);
        });
    }

    fn subscribe_to_pane(
        &mut self,
        pane: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe_in(pane, window, Self::handle_pane_event)
            .detach();
    }

    fn handle_pane_event(
        &mut self,
        pane: &Entity<Pane>,
        event: &pane::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            pane::Event::AddItem { item } => {
                if let Some(workspace) = self.workspace.upgrade() {
                    workspace.update(cx, |workspace, cx| {
                        item.added_to_pane(workspace, pane.clone(), window, cx);
                    });
                }
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            }
            pane::Event::ActivateItem { .. } | pane::Event::Focus => {
                self.active_pane = pane.clone();
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            }
            pane::Event::ChangeItemTitle => {
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            }
            pane::Event::RemovedItem { .. } => {
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            }
            pane::Event::Remove { focus_on_pane } => {
                self.remove_pane_or_close_tab(pane, focus_on_pane.as_ref(), window, cx);
            }
            pane::Event::Split { direction, mode } => {
                if matches!(mode, SplitMode::MovePane) {
                    cx.propagate();
                    return;
                }
                self.duplicate_active_terminal_into_split(*direction, window, cx);
            }
            pane::Event::JoinAll => {
                cx.propagate();
            }
            pane::Event::JoinIntoNext => {
                cx.propagate();
            }
            pane::Event::ZoomIn | pane::Event::ZoomOut => {
                cx.notify();
            }
            pane::Event::UserSavedItem { .. }
            | pane::Event::ItemPinned
            | pane::Event::ItemUnpinned => {}
        }
    }

    fn remove_pane_or_close_tab(
        &mut self,
        pane: &Entity<Pane>,
        focus_on_pane: Option<&Entity<Pane>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.center.panes().len() <= 1 {
            cx.emit(ItemEvent::CloseItem);
            cx.notify();
            return;
        }

        let _ = self.center.remove(pane, cx);
        let pane_to_focus = focus_on_pane
            .cloned()
            .or_else(|| self.center.panes().pop().cloned());
        if let Some(pane_to_focus) = pane_to_focus {
            self.focus_pane(pane_to_focus, window, cx);
        }
        cx.emit(ItemEvent::UpdateTab);
        cx.notify();
    }

    fn duplicate_active_terminal_into_split(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(active_terminal_view) = self.active_terminal_view(cx) else {
            return;
        };
        let terminal = active_terminal_view.read(cx).terminal().clone();
        let working_directory = terminal.read(cx).working_directory();
        let custom_title = active_terminal_view
            .read(cx)
            .custom_title()
            .map(str::to_owned);
        let project = self.project.clone();
        cx.spawn_in(window, async move |this, cx| {
            let terminal = project
                .update(cx, |project, cx| {
                    project.clone_terminal(&terminal, cx, working_directory)
                })
                .await?;
            this.update_in(cx, |this, window, cx| {
                this.split_terminal(terminal, custom_title, direction, window, cx);
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn focus_active_pane(&self, window: &mut Window, cx: &mut App) {
        let pane = self.active_pane.read(cx);
        if let Some(active_item) = pane.active_item() {
            window.focus(&active_item.item_focus_handle(cx), cx);
        } else {
            window.focus(&pane.focus_handle(cx), cx);
        }
    }

    fn focus_pane(&mut self, pane: Entity<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        self.active_pane = pane.clone();
        let focused_item = pane.update(cx, |pane, cx| {
            if pane.active_item().is_some() {
                pane.focus_active_item(window, cx);
                true
            } else {
                false
            }
        });
        if !focused_item {
            window.focus(&pane.focus_handle(cx), cx);
        }
    }

    fn activate_pane_in_direction(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = self
            .center
            .find_pane_in_direction(&self.active_pane, direction, cx)
            .cloned()
        {
            self.focus_pane(pane, window, cx);
            cx.notify();
        }
    }

    fn activate_next_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self.center.panes();
        if let Some(ix) = panes.iter().position(|pane| **pane == self.active_pane) {
            let next_ix = (ix + 1) % panes.len();
            let next_pane = panes[next_ix].clone();
            self.focus_pane(next_pane, window, cx);
            cx.notify();
        }
    }

    fn activate_previous_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self.center.panes();
        if let Some(ix) = panes.iter().position(|pane| **pane == self.active_pane) {
            let prev_ix = cmp::min(ix.wrapping_sub(1), panes.len() - 1);
            let prev_pane = panes[prev_ix].clone();
            self.focus_pane(prev_pane, window, cx);
            cx.notify();
        }
    }

    fn move_pane_to_border(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        if self
            .center
            .move_to_border(&self.active_pane, direction, cx)
            .unwrap_or(false)
        {
            cx.notify();
        }
    }

    fn swap_pane_in_direction(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        if let Some(to) = self
            .center
            .find_pane_in_direction(&self.active_pane, direction, cx)
            .cloned()
        {
            self.center.swap(&self.active_pane, &to, cx);
            cx.notify();
        }
    }
}

impl EventEmitter<ItemEvent> for TerminalTab {}

impl Focusable for TerminalTab {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for TerminalTab {
    type Event = ItemEvent;

    fn tab_content(&self, params: TabContentParams, window: &Window, cx: &App) -> AnyElement {
        if let Some(terminal_view) = self.active_terminal_view(cx) {
            terminal_view.read(cx).tab_content(params, window, cx)
        } else {
            Label::new(self.tab_content_text(params.detail.unwrap_or_default(), cx))
                .color(params.text_color())
                .into_any_element()
        }
    }

    fn tab_content_text(&self, detail: usize, cx: &App) -> SharedString {
        self.active_terminal_view(cx)
            .map(|terminal_view| terminal_view.read(cx).tab_content_text(detail, cx))
            .unwrap_or_else(|| SharedString::new_static("Terminal"))
    }

    fn tab_tooltip_content(&self, cx: &App) -> Option<TabTooltipContent> {
        self.active_terminal_view(cx)
            .and_then(|terminal_view| terminal_view.read(cx).tab_tooltip_content(cx))
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }

    fn act_as_type(
        &self,
        type_id: std::any::TypeId,
        self_handle: &Entity<Self>,
        cx: &App,
    ) -> Option<AnyEntity> {
        if type_id == std::any::TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else if type_id == std::any::TypeId::of::<TerminalView>() {
            self.active_terminal_view(cx).map(Into::into)
        } else {
            None
        }
    }

    fn pixel_position_of_cursor(&self, cx: &App) -> Option<gpui::Point<Pixels>> {
        self.active_pane.read(cx).pixel_position_of_cursor(cx)
    }
}

impl Render for TerminalTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus_handle = self.focus_handle(cx);
        let active_pane = self.active_pane.clone();
        let workspace = self.workspace.clone();
        let decorator = ActivePaneDecorator::new(&active_pane, &workspace);

        div()
            .size_full()
            .track_focus(&focus_handle)
            .on_action(
                cx.listener(|tab, _: &workspace::ActivatePaneLeft, window, cx| {
                    tab.activate_pane_in_direction(SplitDirection::Left, window, cx);
                }),
            )
            .on_action(
                cx.listener(|tab, _: &workspace::ActivatePaneRight, window, cx| {
                    tab.activate_pane_in_direction(SplitDirection::Right, window, cx);
                }),
            )
            .on_action(
                cx.listener(|tab, _: &workspace::ActivatePaneUp, window, cx| {
                    tab.activate_pane_in_direction(SplitDirection::Up, window, cx);
                }),
            )
            .on_action(
                cx.listener(|tab, _: &workspace::ActivatePaneDown, window, cx| {
                    tab.activate_pane_in_direction(SplitDirection::Down, window, cx);
                }),
            )
            .on_action(
                cx.listener(|tab, _: &workspace::ActivateNextPane, window, cx| {
                    tab.activate_next_pane(window, cx);
                }),
            )
            .on_action(
                cx.listener(|tab, _: &workspace::ActivatePreviousPane, window, cx| {
                    tab.activate_previous_pane(window, cx);
                }),
            )
            .on_action(
                cx.listener(|tab, _: &workspace::ActivateLastPane, window, cx| {
                    let pane = tab.center.last_pane();
                    tab.focus_pane(pane, window, cx);
                    cx.notify();
                }),
            )
            .on_action(cx.listener(|tab, _: &workspace::SwapPaneLeft, _, cx| {
                tab.swap_pane_in_direction(SplitDirection::Left, cx);
            }))
            .on_action(cx.listener(|tab, _: &workspace::SwapPaneRight, _, cx| {
                tab.swap_pane_in_direction(SplitDirection::Right, cx);
            }))
            .on_action(cx.listener(|tab, _: &workspace::SwapPaneUp, _, cx| {
                tab.swap_pane_in_direction(SplitDirection::Up, cx);
            }))
            .on_action(cx.listener(|tab, _: &workspace::SwapPaneDown, _, cx| {
                tab.swap_pane_in_direction(SplitDirection::Down, cx);
            }))
            .on_action(cx.listener(|tab, _: &workspace::MovePaneLeft, _, cx| {
                tab.move_pane_to_border(SplitDirection::Left, cx);
            }))
            .on_action(cx.listener(|tab, _: &workspace::MovePaneRight, _, cx| {
                tab.move_pane_to_border(SplitDirection::Right, cx);
            }))
            .on_action(cx.listener(|tab, _: &workspace::MovePaneUp, _, cx| {
                tab.move_pane_to_border(SplitDirection::Up, cx);
            }))
            .on_action(cx.listener(|tab, _: &workspace::MovePaneDown, _, cx| {
                tab.move_pane_to_border(SplitDirection::Down, cx);
            }))
            .child(self.center.render(None, &decorator, window, cx))
    }
}

fn new_terminal_tab_pane(
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    next_timestamp: Arc<AtomicUsize>,
    window: &mut Window,
    cx: &mut Context<TerminalTab>,
) -> Entity<Pane> {
    cx.new(|cx| {
        let mut pane = Pane::new(
            workspace,
            project,
            next_timestamp,
            None,
            crate::NewTerminalTab.boxed_clone(),
            false,
            window,
            cx,
        );
        pane.set_should_display_tab_bar(|_, _| false);
        pane.set_should_display_welcome_page(false);
        pane.set_can_navigate(false, cx);
        pane.display_nav_history_buttons(None);
        pane.set_close_pane_if_empty(true, cx);
        pane.set_zoom_out_on_close(false);
        pane.set_can_split(Some(Arc::new(|_, _, _, _| false)));
        pane
    })
}
