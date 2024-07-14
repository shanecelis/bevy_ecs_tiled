// The code in this file was originally copied from
// [bevy_ecs_tilemap](https://github.com/StarArawn/bevy_ecs_tilemap).
// The original code is licensed under the following license,
// with modifications under the license in the root of this repository.
//
// --
// MIT License

// Copyright (c) 2021 John

// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:

// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.

// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use std::io::{Cursor, Error as IoError, ErrorKind, Read};
use std::path::Path;
use std::sync::Arc;

use bevy::{
    asset::{io::Reader, AssetLoader, AssetPath, AsyncReadExt, LoadContext},
    core::Name,
    hierarchy::BuildChildren,
    log,
    prelude::{
        Asset, AssetApp, AssetEvent, AssetId, Assets, Bundle, Commands, Component,
        DespawnRecursiveExt, Entity, EventReader, GlobalTransform, Handle, Image, Plugin, Query,
        ResMut, Transform, Update, With,
    },
    reflect::TypePath,
    render::view::InheritedVisibility,
    utils::HashMap,
};
use bevy_ecs_tilemap::prelude::*;
use tiled::{ChunkData, FiniteTileLayer, InfiniteTileLayer};

#[derive(Default)]
pub struct TiledMapPlugin;

impl Plugin for TiledMapPlugin {
    fn build(&self, app: &mut bevy::prelude::App) {
        app.init_asset::<TiledMap>()
            .register_asset_loader(TiledLoader)
            .add_systems(Update, process_loaded_maps);
    }
}

#[derive(TypePath, Asset)]
pub struct TiledMap {
    pub map: tiled::Map,

    pub tilemap_textures: HashMap<usize, TilemapTexture>,

    // The offset into the tileset_images for each tile id within each tileset.
    #[cfg(not(feature = "atlas"))]
    pub tile_image_offsets: HashMap<(usize, tiled::TileId), u32>,
}

// Stores a list of tiled layers.
#[derive(Component, Default)]
pub struct TiledLayersStorage {
    pub storage: HashMap<u32, Entity>,
}

/// Marker component for a Tiled map.
#[derive(Component)]
pub struct TiledMapMarker;

/// Marker component for a Tiled map layer.
#[derive(Component)]
pub struct TiledMapLayer {
    // Store the map id so that we can delete layers for this map later.
    // We don't want to store the handle as a component because the parent
    // entity already has it and it complicates queries.
    pub map_handle_id: AssetId<TiledMap>,
}

#[derive(Default, Bundle)]
pub struct TiledMapBundle {
    pub tiled_map: Handle<TiledMap>,
    pub storage: TiledLayersStorage,
    pub transform: Transform,
    pub global_transform: GlobalTransform,
    pub render_settings: TilemapRenderSettings,
}

struct BytesResourceReader<'a, 'b> {
    bytes: Arc<[u8]>,
    context: &'a mut LoadContext<'b>,
}
impl<'a, 'b> BytesResourceReader<'a, 'b> {
    fn new(bytes: &'a [u8], context: &'a mut LoadContext<'b>) -> Self {
        Self {
            bytes: Arc::from(bytes),
            context,
        }
    }
}

impl<'a, 'b> tiled::ResourceReader for BytesResourceReader<'a, 'b> {
    type Resource = Box<dyn Read + 'a>;
    type Error = IoError;

    fn read_from(&mut self, path: &Path) -> std::result::Result<Self::Resource, Self::Error> {
        if let Some(extension) = path.extension() {
            if extension == "tsx" {
                let future = self.context.read_asset_bytes(path.to_path_buf());
                let data = futures_lite::future::block_on(future)
                    .map_err(|err| IoError::new(ErrorKind::NotFound, err))?;
                return Ok(Box::new(Cursor::new(data)));
            }
        }
        Ok(Box::new(Cursor::new(self.bytes.clone())))
    }
}

pub struct TiledLoader;

#[derive(Debug, thiserror::Error)]
pub enum TiledAssetLoaderError {
    /// An [IO](std::io) Error
    #[error("Could not load Tiled file: {0}")]
    Io(#[from] std::io::Error),
}

impl AssetLoader for TiledLoader {
    type Asset = TiledMap;
    type Settings = ();
    type Error = TiledAssetLoaderError;

    async fn load<'a>(
        &'a self,
        reader: &'a mut Reader<'_>,
        _settings: &'a Self::Settings,
        load_context: &'a mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;

        let map_path = load_context.path().to_path_buf();

        let map = {
            // Allow the loader to also load tileset images.
            let mut loader = tiled::Loader::with_cache_and_reader(
                tiled::DefaultResourceCache::new(),
                BytesResourceReader::new(&bytes, load_context),
            );
            // Load the map and all tiles.
            loader.load_tmx_map(&map_path).map_err(|e| {
                std::io::Error::new(ErrorKind::Other, format!("Could not load TMX map: {e}"))
            })?
        };

        let mut tilemap_textures = HashMap::default();
        #[cfg(not(feature = "atlas"))]
        let mut tile_image_offsets = HashMap::default();

        for (tileset_index, tileset) in map.tilesets().iter().enumerate() {
            let tilemap_texture = match &tileset.image {
                None => {
                    #[cfg(feature = "atlas")]
                    {
                        log::info!("Skipping image collection tileset '{}' which is incompatible with atlas feature", tileset.name);
                        continue;
                    }

                    #[cfg(not(feature = "atlas"))]
                    {
                        let mut tile_images: Vec<Handle<Image>> = Vec::new();
                        for (tile_id, tile) in tileset.tiles() {
                            if let Some(img) = &tile.image {
                                // The load context path is the TMX file itself. If the file is at the root of the
                                // assets/ directory structure then the tmx_dir will be empty, which is fine.
                                let tmx_dir = map_path
                                    .parent()
                                    .expect("The asset load context was empty.");
                                let tile_path = tmx_dir.join(&img.source);
                                let asset_path = AssetPath::from(tile_path);
                                log::info!("Loading tile image from {asset_path:?} as image ({tileset_index}, {tile_id})");
                                let texture: Handle<Image> = load_context.load(asset_path.clone());
                                tile_image_offsets
                                    .insert((tileset_index, tile_id), tile_images.len() as u32);
                                tile_images.push(texture.clone());
                            }
                        }

                        TilemapTexture::Vector(tile_images)
                    }
                }
                Some(img) => {
                    // The load context path is the TMX file itself. If the file is at the root of the
                    // assets/ directory structure then the tmx_dir will be empty, which is fine.
                    let tmx_dir = load_context
                        .path()
                        .parent()
                        .expect("The asset load context was empty.");
                    let tile_path = tmx_dir.join(&img.source);
                    let asset_path = AssetPath::from(tile_path);
                    let texture: Handle<Image> = load_context.load(asset_path.clone());

                    TilemapTexture::Single(texture.clone())
                }
            };

            tilemap_textures.insert(tileset_index, tilemap_texture);
        }

        let asset_map = TiledMap {
            map,
            tilemap_textures,
            #[cfg(not(feature = "atlas"))]
            tile_image_offsets,
        };

        log::info!("Loaded map: {}", load_context.path().display());
        Ok(asset_map)
    }

    fn extensions(&self) -> &[&str] {
        static EXTENSIONS: &[&str] = &["tmx"];
        EXTENSIONS
    }
}

/// System to update maps as they are added, changed or removed.
fn process_loaded_maps(
    mut commands: Commands,
    mut map_events: EventReader<AssetEvent<TiledMap>>,
    mut maps: ResMut<Assets<TiledMap>>,
    tile_storage_query: Query<(Entity, &TileStorage)>,
    mut map_query: Query<(
        Entity,
        &Handle<TiledMap>,
        &mut TiledLayersStorage,
        &TilemapRenderSettings,
    )>,
    layer_query: Query<(Entity, &TiledMapLayer), With<TiledMapLayer>>,
) {
    for event in map_events.read() {
        match event {
            AssetEvent::Added { id } => {
                log::info!("Map added: {id}");
                load_map_by_asset_id(
                    &mut commands,
                    &mut maps,
                    &tile_storage_query,
                    &mut map_query,
                    id,
                )
            }
            AssetEvent::Modified { id } => {
                log::info!("Map changed: {id}");
                load_map_by_asset_id(
                    &mut commands,
                    &mut maps,
                    &tile_storage_query,
                    &mut map_query,
                    id,
                )
            }
            AssetEvent::Removed { id } => {
                log::info!("Map removed: {id}");
                remove_map_by_asset_id(
                    &mut commands,
                    &tile_storage_query,
                    &mut map_query,
                    &layer_query,
                    id,
                )
            }
            _ => continue,
        }
    }
}

fn remove_map_by_asset_id(
    mut commands: &mut Commands,
    tile_storage_query: &Query<(Entity, &TileStorage)>,
    map_query: &mut Query<(
        Entity,
        &Handle<TiledMap>,
        &mut TiledLayersStorage,
        &TilemapRenderSettings,
    )>,
    layer_query: &Query<(Entity, &TiledMapLayer), With<TiledMapLayer>>,
    asset_id: &AssetId<TiledMap>,
) {
    log::info!("removing map by asset id: {}", asset_id);
    for (_, map_handle, mut layer_storage, _) in map_query.iter_mut() {
        log::info!("checking layer to remove: {}", map_handle.id());

        // Only process the map that was removed.
        if map_handle.id() != *asset_id {
            continue;
        }

        remove_layers(&mut commands, tile_storage_query, &mut layer_storage);
    }

    // Also manually despawn layers for this map.
    // This is necessary because when a new layer is added, the map handle
    // generation is incremented, and then a subsequent removal event will not
    // match the map_handle in the loop above.
    for (layer_entity, map_layer) in layer_query.iter() {
        // only deal with currently changed map
        if map_layer.map_handle_id != *asset_id {
            continue;
        }

        commands.entity(layer_entity).despawn_recursive();
    }
}

fn remove_layers(
    commands: &mut Commands,
    tile_storage_query: &Query<(Entity, &TileStorage)>,
    layer_storage: &mut TiledLayersStorage,
) {
    for layer_entity in layer_storage.storage.values() {
        if let Ok((_, layer_tile_storage)) = tile_storage_query.get(*layer_entity) {
            for tile in layer_tile_storage.iter().flatten() {
                commands.entity(*tile).despawn_recursive()
            }
        }
        commands.entity(*layer_entity).despawn_recursive();
    }
    layer_storage.storage.clear();
}

fn load_map_by_asset_id(
    mut commands: &mut Commands,
    maps: &mut ResMut<Assets<TiledMap>>,
    tile_storage_query: &Query<(Entity, &TileStorage)>,
    map_query: &mut Query<(
        Entity,
        &Handle<TiledMap>,
        &mut TiledLayersStorage,
        &TilemapRenderSettings,
    )>,
    asset_id: &AssetId<TiledMap>,
) {
    for (map_entity, map_handle, mut layer_storage, render_settings) in map_query.iter_mut() {
        // only deal with currently changed map
        if map_handle.id() != *asset_id {
            continue;
        }

        if let Some(tiled_map) = maps.get(map_handle) {
            remove_layers(&mut commands, tile_storage_query, &mut layer_storage);
            load_map(
                &mut commands,
                &mut layer_storage,
                map_entity,
                map_handle,
                tiled_map,
                render_settings,
            );
        }
    }
}

fn load_map(
    mut commands: &mut Commands,
    layer_storage: &mut TiledLayersStorage,
    map_entity: Entity,
    map_handle: &Handle<TiledMap>,
    tiled_map: &TiledMap,
    render_settings: &TilemapRenderSettings,
) {
    commands
        .entity(map_entity)
        .insert(InheritedVisibility::VISIBLE)
        .insert(Name::new(format!(
            "TiledMap({} x {})",
            tiled_map.map.width, tiled_map.map.height
        )));

    // The TilemapBundle requires that all tile images come exclusively from a single
    // tiled texture or from a Vec of independent per-tile images. Furthermore, all of
    // the per-tile images must be the same size. Since Tiled allows tiles of mixed
    // tilesets on each layer and allows differently-sized tile images in each tileset,
    // this means we need to load each combination of tileset and layer separately.
    for (tileset_index, tileset) in tiled_map.map.tilesets().iter().enumerate() {
        let Some(tilemap_texture) = tiled_map.tilemap_textures.get(&tileset_index) else {
            log::warn!("Skipped creating layer with missing tilemap textures.");
            continue;
        };

        let tile_size = TilemapTileSize {
            x: tileset.tile_width as f32,
            y: tileset.tile_height as f32,
        };

        let tile_spacing = TilemapSpacing {
            x: tileset.spacing as f32,
            y: tileset.spacing as f32,
        };

        // Once materials have been created/added we need to then create the layers.
        for (layer_index, layer) in tiled_map.map.layers().enumerate() {
            let offset_x = layer.offset_x;
            let offset_y = layer.offset_y;

            let tiled::LayerType::Tiles(tile_layer) = layer.layer_type() else {
                log::info!(
                    "Skipping layer {} because only tile layers are supported.",
                    layer.id()
                );
                continue;
            };

            let map_size = TilemapSize {
                x: tiled_map.map.width,
                y: tiled_map.map.height,
            };

            let grid_size = TilemapGridSize {
                x: tiled_map.map.tile_width as f32,
                y: tiled_map.map.tile_height as f32,
            };

            let map_type = match tiled_map.map.orientation {
                tiled::Orientation::Hexagonal => TilemapType::Hexagon(HexCoordSystem::Row),
                tiled::Orientation::Isometric => TilemapType::Isometric(IsoCoordSystem::Diamond),
                tiled::Orientation::Staggered => TilemapType::Isometric(IsoCoordSystem::Staggered),
                tiled::Orientation::Orthogonal => TilemapType::Square,
            };

            let layer_entity = commands
                .spawn(TiledMapLayer {
                    map_handle_id: map_handle.id(),
                })
                .set_parent(map_entity)
                .id();

            let tile_storage = match tile_layer {
                tiled::TileLayer::Finite(layer_data) => load_finite_tiles(
                    &mut commands,
                    layer_entity,
                    map_size,
                    &tiled_map,
                    &layer_data,
                    tileset_index,
                    tilemap_texture,
                ),
                tiled::TileLayer::Infinite(layer_data) => load_infinite_tiles(
                    &mut commands,
                    layer_entity,
                    &tiled_map,
                    &layer_data,
                    tileset_index,
                    tilemap_texture,
                ),
            };

            commands
                .entity(layer_entity)
                .insert(Name::new(format!("TiledMapLayer({})", layer.name)))
                .insert(TilemapBundle {
                    grid_size,
                    size: map_size,
                    storage: tile_storage,
                    texture: tilemap_texture.clone(),
                    tile_size,
                    spacing: tile_spacing,
                    transform: get_tilemap_center_transform(
                        &map_size,
                        &grid_size,
                        &map_type,
                        layer_index as f32,
                    ) * Transform::from_xyz(offset_x, -offset_y, 0.0),
                    map_type,
                    render_settings: *render_settings,
                    ..Default::default()
                })
                .insert(TiledMapMarker);

            layer_storage
                .storage
                .insert(layer_index as u32, layer_entity);
        }
    }
}

fn load_finite_tiles(
    commands: &mut Commands,
    layer_entity: Entity,
    map_size: TilemapSize,
    tiled_map: &TiledMap,
    layer_data: &FiniteTileLayer,
    tileset_index: usize,
    tilemap_texture: &TilemapTexture,
) -> TileStorage {
    let mut tile_storage = TileStorage::empty(map_size);
    for x in 0..map_size.x {
        for y in 0..map_size.y {
            // Transform TMX coords into bevy coords.
            let mapped_y = tiled_map.map.height - 1 - y;

            let mapped_x = x as i32;
            let mapped_y = mapped_y as i32;

            let layer_tile = match layer_data.get_tile(mapped_x, mapped_y) {
                Some(t) => t,
                None => {
                    continue;
                }
            };
            if tileset_index != layer_tile.tileset_index() {
                continue;
            }
            let layer_tile_data = match layer_data.get_tile_data(mapped_x, mapped_y) {
                Some(d) => d,
                None => {
                    continue;
                }
            };

            let texture_index = match tilemap_texture {
                TilemapTexture::Single(_) => layer_tile.id(),
                #[cfg(not(feature = "atlas"))]
                TilemapTexture::Vector(_) =>
                    *tiled_map.tile_image_offsets.get(&(tileset_index, layer_tile.id()))
                    .expect("The offset into to image vector should have been saved during the initial load."),
                #[cfg(not(feature = "atlas"))]
                _ => unreachable!()
            };

            let tile_pos = TilePos { x, y };
            let tile_entity = commands
                .spawn(TileBundle {
                    position: tile_pos,
                    tilemap_id: TilemapId(layer_entity),
                    texture_index: TileTextureIndex(texture_index),
                    flip: TileFlip {
                        x: layer_tile_data.flip_h,
                        y: layer_tile_data.flip_v,
                        d: layer_tile_data.flip_d,
                    },
                    ..Default::default()
                })
                .set_parent(layer_entity)
                .insert(Name::new(format!(
                    "TiledMapTile({},{})",
                    tile_pos.x, tile_pos.y
                )))
                .id();
            tile_storage.set(&tile_pos, tile_entity);
        }
    }

    tile_storage
}

fn load_infinite_tiles(
    commands: &mut Commands,
    layer_entity: Entity,
    tiled_map: &TiledMap,
    infinite_layer: &InfiniteTileLayer,
    tileset_index: usize,
    tilemap_texture: &TilemapTexture,
) -> TileStorage {
    // Determine top left coordinate so we can offset the map.
    let (topleft_x, topleft_y) = infinite_layer
        .chunks()
        .fold((0, 0), |acc, (pos, _)| (acc.0.min(pos.0), acc.1.min(pos.1)));

    let (bottomright_x, bottomright_y) = infinite_layer
        .chunks()
        .fold((0, 0), |acc, (pos, _)| (acc.0.max(pos.0), acc.1.max(pos.1)));

    // TODO: Provide a way to surface the origin point (the point that was 0,0 in Tiled)
    //       to the caller.
    let _origin = (
        -topleft_x * ChunkData::WIDTH as i32,
        -topleft_y * ChunkData::HEIGHT as i32,
    );

    // Recalculate map size based on the top left and bottom right coordinates.
    let map_size = TilemapSize {
        x: (bottomright_x - topleft_x + 1) as u32 * ChunkData::WIDTH as u32,
        y: (bottomright_y - topleft_y + 1) as u32 * ChunkData::HEIGHT as u32,
    };
    let mut tile_storage = TileStorage::empty(map_size);

    for (chunk_pos, chunk) in infinite_layer.chunks() {
        // bevy_ecs_tilemap doesn't support negative tile coordinates, so shift all chunks
        // such that the top-left chunk is at (0, 0).
        let chunk_pos_mapped = (chunk_pos.0 - topleft_x, chunk_pos.1 - topleft_y);

        for x in 0..ChunkData::WIDTH as u32 {
            for y in 0..ChunkData::HEIGHT as u32 {
                // Invert y to match bevy coordinates.
                let layer_tile = match chunk.get_tile(x as i32, y as i32) {
                    Some(t) => t,
                    None => {
                        continue;
                    }
                };
                if tileset_index != layer_tile.tileset_index() {
                    continue;
                }

                let layer_tile_data = match chunk.get_tile_data(x as i32, y as i32) {
                    Some(d) => d,
                    None => {
                        continue;
                    }
                };

                let (tile_x, tile_y) = (
                    chunk_pos_mapped.0 * ChunkData::WIDTH as i32 + x as i32,
                    chunk_pos_mapped.1 * ChunkData::HEIGHT as i32 + y as i32,
                );

                let texture_index = match tilemap_texture {
                                            TilemapTexture::Single(_) => layer_tile.id(),
                                            #[cfg(not(feature = "atlas"))]
                                            TilemapTexture::Vector(_) =>
                                                *tiled_map.tile_image_offsets.get(&(tileset_index, layer_tile.id()))
                                                .expect("The offset into to image vector should have been saved during the initial load."),
                                            #[cfg(not(feature = "atlas"))]
                                            _ => unreachable!()
                                        };

                let tile_pos = TilePos {
                    x: tile_x as u32,
                    y: map_size.y - tile_y as u32,
                };
                let tile_entity = commands
                    .spawn(TileBundle {
                        position: tile_pos,
                        tilemap_id: TilemapId(layer_entity),
                        texture_index: TileTextureIndex(texture_index),
                        flip: TileFlip {
                            x: layer_tile_data.flip_h,
                            y: layer_tile_data.flip_v,
                            d: layer_tile_data.flip_d,
                        },
                        ..Default::default()
                    })
                    .set_parent(layer_entity)
                    .insert(Name::new(format!("Tile({},{})", tile_pos.x, tile_pos.y)))
                    .id();

                tile_storage.set(&tile_pos, tile_entity);
            }
        }
    }

    tile_storage
}
