//! This module handles all the logic related to loading and spawning Tiled worlds.

pub mod asset;
pub mod components;
pub mod events;

/// `bevy_ecs_tiled` world related public exports
pub mod prelude {
    pub use super::asset::*;
    pub use super::components::*;
    pub use super::events::*;
    pub use super::TiledWorldHandle;
}

use crate::prelude::*;
use bevy::{
    asset::RecursiveDependencyLoadState,
    math::bounding::{Aabb2d, IntersectsVolume},
    prelude::*,
};
use bevy_ecs_tilemap::map::TilemapRenderSettings;

/// Wrapper around the [Handle] to the `.world` file representing the [TiledWorld].
///
/// This is the main [Component] that must be spawned to load a Tiled world.
#[derive(Component, Reflect)]
#[require(
    TiledWorldStorage,
    TiledWorldSettings,
    TiledMapSettings,
    TilemapRenderSettings,
    Visibility,
    Transform
)]
pub struct TiledWorldHandle(pub Handle<TiledWorld>);

pub(crate) fn build(app: &mut bevy::prelude::App) {
    app.init_asset::<TiledWorld>()
        .init_asset_loader::<TiledWorldLoader>()
        .register_type::<TiledWorldHandle>()
        .register_type::<TiledWorldSettings>()
        .register_type::<TiledWorldStorage>()
        .add_event::<TiledWorldCreated>()
        .add_systems(PreUpdate, process_loaded_worlds)
        .add_systems(
            PostUpdate,
            (
                world_chunking.before(handle_world_events),
                handle_world_events,
            ),
        );
}

#[allow(clippy::type_complexity)]
fn world_chunking(
    camera_query: Query<&Transform, (With<Camera>, Changed<Transform>)>,
    worlds: Res<Assets<TiledWorld>>,
    asset_server: Res<AssetServer>,
    mut commands: Commands,
    mut world_query: Query<
        (
            Entity,
            &TiledWorldHandle,
            &GlobalTransform,
            &TiledWorldSettings,
            &TiledMapSettings,
            &TilemapRenderSettings,
            &mut TiledWorldStorage,
        ),
        With<TiledWorldMarker>,
    >,
) {
    for (
        world_entity,
        world_handle,
        world_transform,
        world_settings,
        map_settings,
        render_settings,
        mut storage,
    ) in world_query.iter_mut()
    {
        // Make sure we have a valid reference on a fully loaded world asset
        let Some(tiled_world) = asset_server
            .get_recursive_dependency_load_state(&world_handle.0)
            .and_then(|state| {
                if state.is_loaded() {
                    return worlds.get(&world_handle.0);
                }
                None
            })
        else {
            continue;
        };

        let mut to_remove = Vec::new();
        let mut to_spawn = Vec::new();

        if let Some(chunking) = world_settings.chunking {
            let mut visible_maps = Vec::new();
            let cameras: Vec<Aabb2d> = camera_query
                .iter()
                .map(|transform| {
                    Aabb2d::new(
                        Vec2::new(transform.translation.x, transform.translation.y),
                        chunking,
                    )
                })
                .collect();
            // Check which map is visible by testing them against each camera (if there are multiple)
            // If map aabb overlaps with the camera_view, it is visible
            for_each_map(tiled_world, world_transform, |idx, aabb| {
                for c in cameras.iter() {
                    if aabb.intersects(c) {
                        visible_maps.push(idx);
                    }
                }
            });

            // All the maps that are visible but not already spawned should be spawned
            for idx in visible_maps.iter() {
                if !storage.spawned_maps.contains_key(idx) {
                    to_spawn.push(*idx);
                }
            }

            // All the maps that are spawned but not visible should be removed
            for (idx, _) in storage.spawned_maps.iter() {
                if !visible_maps.iter().any(|i| i == idx) {
                    to_remove.push(*idx);
                }
            }
        } else if storage.spawned_maps.is_empty() {
            // No chunking and we don't have spawned any map yet: just spawn all maps
            for idx in 0..tiled_world.maps.len() - 1 {
                to_spawn.push(idx);
            }
        }

        // Despawn maps
        for idx in to_remove {
            if let Some(map_entity) = storage.spawned_maps.remove(&idx) {
                debug!("Despawning map (entity = {:?})", map_entity);
                commands.entity(map_entity).despawn_recursive();
            }
        }

        // Spawn maps
        for idx in to_spawn {
            let Some((rect, handle)) = tiled_world.maps.get(idx) else {
                continue;
            };
            let map_entity = commands
                .spawn((
                    TiledMapHandle(handle.clone_weak()),
                    Transform::from_translation(Vec3::new(rect.min.x, rect.min.y, 0.0)),
                    TiledMapSettings {
                        layer_positioning: LayerPositioning::TiledOffset,
                        ..*map_settings
                    },
                    *render_settings,
                ))
                .set_parent(world_entity)
                .id();
            debug!(
                "Spawned map (handle = {:?} / entity = {:?})",
                handle, map_entity
            );
            storage.spawned_maps.insert(idx, map_entity);
        }
    }
}

/// System to spawn a world once it has been fully loaded.
#[allow(clippy::type_complexity)]
fn process_loaded_worlds(
    asset_server: Res<AssetServer>,
    mut commands: Commands,
    worlds: Res<Assets<TiledWorld>>,
    mut world_query: Query<
        (
            Entity,
            &TiledWorldHandle,
            &TiledMapSettings,
            &mut Transform,
            &mut TiledWorldStorage,
        ),
        Or<(Changed<TiledWorldHandle>, With<RespawnTiledWorld>)>,
    >,
    mut world_event: EventWriter<TiledWorldCreated>,
) {
    for (world_entity, world_handle, map_settings, mut world_transform, mut world_storage) in
        world_query.iter_mut()
    {
        if let Some(load_state) = asset_server.get_recursive_dependency_load_state(&world_handle.0)
        {
            if !load_state.is_loaded() {
                if let RecursiveDependencyLoadState::Failed(_) = load_state {
                    error!(
                        "World failed to load, despawn it (handle = {:?} / entity = {:?})",
                        world_handle.0, world_entity
                    );
                    commands.entity(world_entity).despawn_recursive();
                } else {
                    // If not fully loaded yet, insert the 'Respawn' marker so we will try to load it at next frame
                    debug!(
                        "World is not fully loaded yet, will try again next frame (handle = {:?} / entity = {:?})",
                        world_handle.0, world_entity
                    );
                    commands.entity(world_entity).insert(RespawnTiledWorld);
                }
                continue;
            }

            // World should be loaded at this point
            let Some(tiled_world) = worlds.get(&world_handle.0) else {
                error!("Cannot get a valid TiledWorld out of Handle<TiledWorld>: has the last strong reference to the asset been dropped ? (handle = {:?} / entity = {:?})", world_handle.0, world_entity);
                commands.entity(world_entity).despawn_recursive();
                continue;
            };

            debug!(
                "World has finished loading, spawn it (handle = {:?} / entity = {:?})",
                world_handle.0, world_entity
            );

            // Clean previous maps before trying to spawn the new ones
            remove_maps(&mut commands, &mut world_storage);

            // Adjust world transform if needed
            if let LayerPositioning::Centered = map_settings.layer_positioning {
                world_transform.translation += Vec3::new(
                    -tiled_world.world_rect.max.x / 2.0,
                    -tiled_world.world_rect.max.y / 2.0,
                    0.0,
                );
            }

            // Remove the 'Respawn' marker and insert additional components
            commands
                .entity(world_entity)
                .insert((
                    Name::new(format!(
                        "TiledWorld: {}",
                        tiled_world.world.source.display()
                    )),
                    TiledWorldMarker,
                ))
                .remove::<RespawnTiledWorld>();

            let event = TiledWorldCreated {
                entity: world_entity,
                asset_id: world_handle.0.id(),
            };
            commands.trigger_targets(event.clone(), world_entity);
            world_event.send(event);
        }
    }
}

/// System to update worlds as they are changed or removed.
fn handle_world_events(
    mut commands: Commands,
    mut world_events: EventReader<AssetEvent<TiledWorld>>,
    world_query: Query<(Entity, &TiledWorldHandle)>,
) {
    for event in world_events.read() {
        match event {
            AssetEvent::Modified { id } => {
                info!("World changed: {id}");
                for (world_entity, world_handle) in world_query.iter() {
                    if world_handle.0.id() == *id {
                        commands.entity(world_entity).insert(RespawnTiledWorld);
                    }
                }
            }
            AssetEvent::Removed { id } => {
                info!("World removed: {id}");
                for (world_entity, world_handle) in world_query.iter() {
                    if world_handle.0.id() == *id {
                        commands.entity(world_entity).despawn_recursive();
                    }
                }
            }
            _ => continue,
        }
    }
}

fn remove_maps(commands: &mut Commands, world_storage: &mut TiledWorldStorage) {
    for (_, map_entity) in world_storage.spawned_maps.iter() {
        commands.entity(*map_entity).despawn_recursive();
    }
    world_storage.spawned_maps.clear();
}

pub(crate) fn for_each_map<F: FnMut(usize, Aabb2d)>(
    tiled_world: &TiledWorld,
    world_transform: &GlobalTransform,
    mut f: F,
) {
    let (_, r, t) = world_transform.to_scale_rotation_translation();
    let (axis, mut angle) = r.to_axis_angle();
    if axis.z < 0. {
        angle = -angle;
    }
    let world_isometry = Isometry2d::new(Vec2::new(t.x, t.y), Rot2::radians(angle));
    for (idx, (rect, _)) in tiled_world.maps.iter().enumerate() {
        f(
            idx,
            Aabb2d::from_point_cloud(
                Isometry2d::IDENTITY,
                &[
                    world_isometry.transform_point(Vec2::new(rect.min.x, rect.min.y)),
                    world_isometry.transform_point(Vec2::new(rect.min.x, rect.max.y)),
                    world_isometry.transform_point(Vec2::new(rect.max.x, rect.max.y)),
                    world_isometry.transform_point(Vec2::new(rect.max.x, rect.min.y)),
                ],
            ),
        );
    }
}
