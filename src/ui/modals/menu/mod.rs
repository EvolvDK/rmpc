use anyhow::Result;
use input_section::InputSection;
use list_section::ListSection;
use modal::MenuModal;
use multi_action_section::MultiActionSection;
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    widgets::Widget,
};

use crate::{
    config::keys::actions::AddOpts,
    ctx::Ctx,
    shared::{
        events::AppEvent,
        key_event::KeyEvent,
        macros::{status_error, status_info},
        mpd_client_ext::{Enqueue, MpdClientExt as _},
    },
    ui::{modals::input_modal::InputModal, UiAppEvent},
    youtube::storage::{self, PlaylistItem},
};

mod input_section;
mod list_section;
pub mod modal;
mod multi_action_section;

trait Section {
    fn down(&mut self) -> bool;
    fn up(&mut self) -> bool;
    fn right(&mut self) -> bool {
        true
    }
    fn left(&mut self) -> bool {
        true
    }
    fn unselect(&mut self);
    fn unfocus(&mut self) {}

    fn confirm(&mut self, ctx: &Ctx) -> Result<bool>;
    fn key_input(&mut self, _key: &mut KeyEvent, _ctx: &Ctx) -> Result<()> {
        Ok(())
    }

    fn len(&self) -> usize;
    fn render(&mut self, area: Rect, buf: &mut Buffer);

    fn left_click(&mut self, pos: ratatui::layout::Position);
    fn double_click(&mut self, pos: ratatui::layout::Position, ctx: &Ctx) -> Result<bool>;
}

#[derive(Debug)]
enum SectionType<'a> {
    Menu(ListSection),
    Multi(MultiActionSection<'a>),
    Input(InputSection<'a>),
}

impl Section for SectionType<'_> {
    fn down(&mut self) -> bool {
        match self {
            SectionType::Menu(s) => s.down(),
            SectionType::Multi(s) => s.down(),
            SectionType::Input(s) => s.down(),
        }
    }

    fn up(&mut self) -> bool {
        match self {
            SectionType::Menu(s) => s.up(),
            SectionType::Multi(s) => s.up(),
            SectionType::Input(s) => s.up(),
        }
    }

    fn right(&mut self) -> bool {
        match self {
            SectionType::Menu(s) => s.right(),
            SectionType::Multi(s) => s.right(),
            SectionType::Input(s) => s.right(),
        }
    }

    fn left(&mut self) -> bool {
        match self {
            SectionType::Menu(s) => s.left(),
            SectionType::Multi(s) => s.left(),
            SectionType::Input(s) => s.left(),
        }
    }

    fn unselect(&mut self) {
        match self {
            SectionType::Menu(s) => s.unselect(),
            SectionType::Multi(s) => s.unselect(),
            SectionType::Input(s) => s.unselect(),
        }
    }

    fn unfocus(&mut self) {
        match self {
            SectionType::Menu(s) => s.unfocus(),
            SectionType::Multi(s) => s.unfocus(),
            SectionType::Input(s) => s.unfocus(),
        }
    }

    fn confirm(&mut self, ctx: &Ctx) -> Result<bool> {
        match self {
            SectionType::Menu(s) => s.confirm(ctx),
            SectionType::Multi(s) => s.confirm(ctx),
            SectionType::Input(s) => s.confirm(ctx),
        }
    }

    fn len(&self) -> usize {
        match self {
            SectionType::Menu(s) => s.len(),
            SectionType::Multi(s) => s.len(),
            SectionType::Input(s) => s.len(),
        }
    }

    fn render(&mut self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        match self {
            SectionType::Menu(s) => Widget::render(s, area, buf),
            SectionType::Multi(s) => Widget::render(s, area, buf),
            SectionType::Input(s) => Widget::render(s, area, buf),
        }
    }

    fn key_input(&mut self, key: &mut KeyEvent, ctx: &Ctx) -> Result<()> {
        match self {
            SectionType::Menu(s) => s.key_input(key, ctx),
            SectionType::Multi(s) => s.key_input(key, ctx),
            SectionType::Input(s) => s.key_input(key, ctx),
        }
    }

    fn left_click(&mut self, pos: Position) {
        match self {
            SectionType::Menu(s) => s.left_click(pos),
            SectionType::Multi(s) => s.left_click(pos),
            SectionType::Input(s) => s.left_click(pos),
        }
    }

    fn double_click(&mut self, pos: Position, ctx: &Ctx) -> Result<bool> {
        match self {
            SectionType::Menu(s) => s.double_click(pos, ctx),
            SectionType::Multi(s) => s.double_click(pos, ctx),
            SectionType::Input(s) => s.double_click(pos, ctx),
        }
    }
}

pub fn create_add_modal<'a>(
    opts: Vec<(String, AddOpts, (Vec<Enqueue>, Option<usize>))>,
    ctx: &Ctx,
) -> MenuModal<'a> {
    MenuModal::new(ctx)
        .list_section(ctx, |section| {
            let queue_len = ctx.queue.len();
            let current_song_idx = ctx.find_current_song_in_queue().map(|(i, _)| i);
            let mut section = section;

            for (label, options, (enqueue, hovered_idx)) in opts {
                section = section.item(label, move |ctx| {
                    if !enqueue.is_empty() {
                        ctx.command(move |client| {
                            let autoplay =
                                options.autoplay(queue_len, current_song_idx, hovered_idx);
                            client.enqueue_multiple(enqueue, options.position, autoplay)?;

                            Ok(())
                        });
                    }
                    Ok(())
                });
            }
            Some(section)
        })
        .list_section(ctx, |section| Some(section.item("Cancel", |_ctx| Ok(()))))
        .build()
}

pub fn queue_actions(
    items: Vec<PlaylistItem>,
) -> impl FnOnce(ListSection) -> Option<ListSection> {
    move |section| {
        let add_items = items.clone();
        Some(
            section
                .item("Add to queue", move |ctx| {
                    ctx.app_event_sender.send(AppEvent::UiEvent(
                        UiAppEvent::AddPlaylistItemsToQueue(add_items),
                    ))?;
                    Ok(())
                })
                .item("Replace queue", move |ctx| {
                    ctx.app_event_sender.send(AppEvent::UiEvent(
                        UiAppEvent::ReplaceQueueWithPlaylistItems(items),
                    ))?;
                    Ok(())
                }),
        )
    }
}

pub fn playlist_management_actions(
    items: Vec<PlaylistItem>,
    playlist_name: String,
) -> impl FnOnce(ListSection) -> Option<ListSection> {
    move |section| {
        let remove_items = items.clone();
        let create_playlist_items = items;
        let p_name_for_remove = playlist_name;
        Some(
            section
                .item("Remove from playlist", move |ctx| {
                    let p_name = p_name_for_remove.clone();
                    let modal = modal::MenuModal::new(ctx)
                        .list_section(ctx, |s| {
                            Some(
                                s.item(
                                    format!("Yes, remove {} items", remove_items.len()),
                                    move |ctx| {
                                        match (|| -> Result<()> {
                                            let mut content = storage::load_playlist(&p_name)?;
                                            content.retain(|item| !remove_items.contains(item));
                                            storage::save_playlist(&p_name, &content)?;
                                            Ok(())
                                        })() {
                                            Ok(_) => {
                                                ctx.app_event_sender.send(AppEvent::UiEvent(
                                                    UiAppEvent::RefreshRmpcPlaylists,
                                                ))?;
                                                status_info!(
                                                    "Removed {} items from '{}'",
                                                    remove_items.len(),
                                                    p_name
                                                );
                                            }
                                            Err(e) => {
                                                status_error!("Failed to remove items: {e}")
                                            }
                                        }
                                        Ok(())
                                    },
                                )
                                .item("No, cancel", |_| Ok(())),
                            )
                        })
                        .build();
                    ctx.app_event_sender
                        .send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?;
                    Ok(())
                })
                .item("Create playlist from selection...", move |ctx| {
                    let modal = InputModal::new(ctx)
                        .title("Create new playlist")
                        .on_confirm(move |ctx, name| {
                            if !name.is_empty() {
                                ctx.app_event_sender.send(AppEvent::UiEvent(
                                    UiAppEvent::CreatePlaylistFromItems {
                                        name: name.to_string(),
                                        items: create_playlist_items,
                                    },
                                ))?;
                            }
                            Ok(())
                        });
                    ctx.app_event_sender
                        .send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?;
                    Ok(())
                }),
        )
    }
}

pub fn add_to_playlist_actions(
    items: Vec<PlaylistItem>,
    playlists: Vec<String>,
) -> impl FnOnce(ListSection) -> Option<ListSection> {
    move |section| {
        if playlists.is_empty() {
            return None;
        }
        Some(section.item("Add to playlist...", move |ctx| {
            let modal = modal::MenuModal::new(ctx)
                .list_section(ctx, |s| {
                    let mut s = s;
                    for p_name in playlists {
                        let items_to_add = items.clone();
                        let target_p_name = p_name.clone();
                        s = s.item(p_name, move |ctx| {
                            ctx.app_event_sender
                                .send(AppEvent::UiEvent(UiAppEvent::AddItemsToPlaylist {
                                    name: target_p_name,
                                    items: items_to_add,
                                }))?;
                            Ok(())
                        });
                    }
                    Some(s)
                })
                .list_section(ctx, |s| Some(s.item("Cancel", |_| Ok(()))))
                .build();
            ctx.app_event_sender
                .send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?;
            Ok(())
        }))
    }
}

pub fn create_playlist_action(
    items: Vec<PlaylistItem>,
) -> impl FnOnce(ListSection) -> Option<ListSection> {
    move |section| {
        if items.is_empty() {
            return None;
        }
        Some(section.item("Create playlist from selection...", move |ctx| {
            let modal = InputModal::new(ctx)
                .title("Create new playlist")
                .on_confirm(move |ctx, name| {
                    if !name.is_empty() {
                        ctx.app_event_sender.send(AppEvent::UiEvent(
                            UiAppEvent::CreatePlaylistFromItems {
                                name: name.to_string(),
                                items,
                            },
                        ))?;
                    }
                    Ok(())
                });
            ctx.app_event_sender
                .send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?;
            Ok(())
        }))
    }
}

pub fn playlist_queue_actions(
    playlist_name: String,
) -> impl FnOnce(ListSection) -> Option<ListSection> {
    move |section| {
        let p_name_add = playlist_name.clone();
        let p_name_replace = playlist_name;
        Some(
            section
                .item("Add to queue", move |ctx| {
                    match storage::load_playlist(&p_name_add) {
                        Ok(items) => ctx.app_event_sender.send(AppEvent::UiEvent(
                            UiAppEvent::AddPlaylistItemsToQueue(items),
                        ))?,
                        Err(e) => status_error!("Failed to load playlist: {e}"),
                    }
                    Ok(())
                })
                .item("Replace queue", move |ctx| {
                    match storage::load_playlist(&p_name_replace) {
                        Ok(items) => ctx.app_event_sender.send(AppEvent::UiEvent(
                            UiAppEvent::ReplaceQueueWithPlaylistItems(items),
                        ))?,
                        Err(e) => status_error!("Failed to load playlist: {e}"),
                    }
                    Ok(())
                }),
        )
    }
}

pub fn youtube_library_actions(
    videos: Vec<crate::youtube::YouTubeVideo>,
) -> impl FnOnce(ListSection) -> Option<ListSection> {
    move |section| {
        Some(section.item("Remove from library", move |ctx| {
            let modal = modal::MenuModal::new(ctx)
                .list_section(ctx, |s| {
                    Some(
                        s.item(format!("Yes, remove {} item(s)", videos.len()), {
                            let videos = videos.clone();
                            move |ctx| {
                                for video in videos {
                                    ctx.app_event_sender.send(AppEvent::UiEvent(
                                        UiAppEvent::YouTubeLibraryRemoveVideo(video.id),
                                    ))?;
                                }
                                Ok(())
                            }
                        })
                        .item("No, cancel", |_| Ok(())),
                    )
                })
                .build();
            ctx.app_event_sender
                .send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?;
            Ok(())
        }))
    }
}

pub fn add_playlist_to_playlist_actions(
    source_playlist_name: String,
    other_playlists: Vec<String>,
) -> impl FnOnce(ListSection) -> Option<ListSection> {
    move |section| {
        if other_playlists.is_empty() {
            return None;
        }
        Some(section.item("Add to playlist...", move |ctx| {
            let modal = modal::MenuModal::new(ctx)
                .list_section(ctx, |s| {
                    let mut s = s;
                    for p_name in other_playlists {
                        let source_p_name = source_playlist_name.clone();
                        let target_p_name = p_name.clone();
                        s = s.item(p_name, move |ctx| {
                            match storage::load_playlist(&source_p_name) {
                                Ok(source_content) => {
                                    ctx.app_event_sender.send(AppEvent::UiEvent(
                                        UiAppEvent::AddItemsToPlaylist {
                                            name: target_p_name,
                                            items: source_content,
                                        },
                                    ))?;
                                }
                                Err(e) => {
                                    status_error!("Failed to load playlist to add: {e}")
                                }
                            }
                            Ok(())
                        });
                    }
                    Some(s)
                })
                .list_section(ctx, |s| Some(s.item("Cancel", |_| Ok(()))))
                .build();
            ctx.app_event_sender
                .send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?;
            Ok(())
        }))
    }
}

pub fn playlist_cloning_actions(
    playlist_name: String,
) -> impl FnOnce(ListSection) -> Option<ListSection> {
    move |section| {
        let p_name = playlist_name;
        Some(section.item("Create a copy...", move |ctx| {
            let modal = InputModal::new(ctx)
                .title("Create a copy")
                .on_confirm(move |ctx, new_name| {
                    if !new_name.is_empty() {
                        match (|| -> Result<()> {
                            let content = storage::load_playlist(&p_name)?;
                            storage::save_playlist(new_name, &content)?;
                            Ok(())
                        })() {
                            Ok(_) => {
                                ctx.app_event_sender
                                    .send(AppEvent::UiEvent(UiAppEvent::RefreshRmpcPlaylists))?;
                                status_info!("Created playlist '{}'", new_name);
                            }
                            Err(e) => status_error!("Failed to create playlist copy: {e}"),
                        }
                    }
                    Ok(())
                });
            ctx.app_event_sender
                .send(AppEvent::UiEvent(UiAppEvent::Modal(Box::new(modal))))?;
            Ok(())
        }))
    }
}
