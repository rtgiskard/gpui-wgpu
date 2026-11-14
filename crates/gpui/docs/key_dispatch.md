# Key Dispatch

GPUI is designed for keyboard-first interactivity.

To expose functionality to the mouse, you render a button with a click handler.

To expose functionality to the keyboard, you bind an _action_ in a _key context_.

Actions are similar to framework-level events like `MouseDown`, `KeyDown`, etc, but you can define them yourself:

```rust
mod menu {
    #[derive(Clone, PartialEq, Default, gpui::Action)]
    #[action(namespace = menu, no_json)]
    struct MoveUp;

    #[derive(Clone, PartialEq, Default, gpui::Action)]
    #[action(namespace = menu, no_json)]
    struct MoveDown;
}
```

For unit actions with JSON keymap support, use the macro:

```rust
mod menu {
    gpui::actions!(menu, [MoveUp, MoveDown]);
}
```

Actions can also be more complex types:

```rust
mod menu {
    #[derive(Clone, PartialEq, serde::Deserialize, schemars::JsonSchema, gpui::Action)]
    #[action(namespace = menu)]
    struct Move {
        direction: Direction,
        select: bool,
    }
}
```

To bind actions, chain `on_action` on to your element:

```rust
impl Render for Menu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .on_action(cx.listener(|this: &mut Menu, action: &MoveUp, window, cx| {
                // ...
            }))
            .on_action(cx.listener(|this, action: &MoveDown, window, cx| {
                // ...
            }))
            .children(unimplemented!())
    }
}
```

In order to bind keys to actions, you need to declare a _key context_ for part of the element tree by calling `key_context`.

```rust
impl Render for Menu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("menu")
            .on_action(cx.listener(|this: &mut Menu, action: &MoveUp, window, cx| {
                // ...
            }))
            .on_action(cx.listener(|this, action: &MoveDown, window, cx| {
                // ...
            }))
            .children(unimplemented!())
    }
}
```

Register standalone application bindings with `App::bind_keys` and `KeyBinding::new`.
The following JSON is the host application's keymap representation (as used by Zed),
not a file that GPUI loads automatically. Action names use the declared namespace.

```json
{
  "context": "menu",
  "bindings": {
    "up": "menu::MoveUp",
    "down": "menu::MoveDown"
  }
}
```

If you had opted for the more complex type definition, you'd provide the serialized representation of the action alongside the name:

```json
{
  "context": "menu",
  "bindings": {
    "up": ["menu::Move", {"direction": "up", "select": false}],
    "down": ["menu::Move", {"direction": "down", "select": false}],
    "shift-up": ["menu::Move", {"direction": "up", "select": true}],
    "shift-down": ["menu::Move", {"direction": "down", "select": true}]
  }
}
```
