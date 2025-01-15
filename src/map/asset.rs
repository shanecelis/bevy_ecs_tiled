//! This module contains all map [Asset]s definition.

use std::io::ErrorKind;
#[cfg(feature = "user_properties")]
use std::ops::Deref;

#[cfg(feature = "user_properties")]
use bevy::reflect::TypeRegistryArc;

#[cfg(feature = "user_properties")]
use crate::properties::load::DeserializedMapProperties;

use crate::{cache::TiledResourceCache, reader::BytesResourceReader};

use bevy::{
    asset::{io::Reader, AssetLoader, AssetPath, LoadContext, LoadedAsset},
    prelude::*,
    utils::HashMap,
};

use bevy_ecs_tilemap::prelude::*;

/// Tiled map `Asset`.
///
/// `Asset` holding Tiled map informations.
#[derive(TypePath, Asset)]
pub struct TiledMap {
    /// The raw Tiled map
    pub map: tiled::Map,
    /// HashMap of the map tilesets.
    ///
    /// Key is the Tiled tileset index.
    pub tilesets: HashMap<usize, TiledMapTileset>,
    /// Map properties
    #[cfg(feature = "user_properties")]
    pub(crate) properties: DeserializedMapProperties,
}

#[derive(Default)]
pub struct TiledMapTileset {
    /// Does this tileset can be used for tiles layer ?
    ///
    /// A tileset can be used for tiles layer only if all the images it contains have the
    /// same dimensions (restriction from bevy_ecs_tilemap).
    pub usable_for_tiles_layer: bool,
    /// Tileset texture (ie. a single image or an images collection)
    pub tilemap_texture: TilemapTexture,
    /// The [TextureAtlasLayout] handle associated to each tileset, if any.
    pub texture_atlas_layout_handle: Option<Handle<TextureAtlasLayout>>,
    /// The offset into the tileset_images for each tile id within each tileset.
    #[cfg(not(feature = "atlas"))]
    pub tile_image_offsets: HashMap<tiled::TileId, u32>,
}

pub(crate) struct TiledMapLoader {
    pub cache: TiledResourceCache,
    #[cfg(feature = "user_properties")]
    pub registry: TypeRegistryArc,
}

impl FromWorld for TiledMapLoader {
    fn from_world(world: &mut World) -> Self {
        Self {
            cache: world.resource::<TiledResourceCache>().clone(),
            #[cfg(feature = "user_properties")]
            registry: world.resource::<AppTypeRegistry>().0.clone(),
        }
    }
}

/// [TiledMap] loading error.
#[derive(Debug, thiserror::Error)]
pub enum TiledMapLoaderError {
    /// An [IO](std::io) Error
    #[error("Could not load Tiled file: {0}")]
    Io(#[from] std::io::Error),
}

impl AssetLoader for TiledMapLoader {
    type Asset = TiledMap;
    type Settings = ();
    type Error = TiledMapLoaderError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;

        debug!("Start loading map '{}'", load_context.path().display());

        let map_path = load_context.path().to_path_buf();
        let map = {
            // Allow the loader to also load tileset images.
            let mut loader = tiled::Loader::with_cache_and_reader(
                self.cache.clone(),
                BytesResourceReader::new(&bytes, load_context),
            );
            // Load the map and all tiles.
            loader.load_tmx_map(&map_path).map_err(|e| {
                std::io::Error::new(ErrorKind::Other, format!("Could not load TMX map: {e}"))
            })?
        };

        let mut tilesets = HashMap::default();
        for (tileset_index, tileset) in map.tilesets().iter().enumerate() {
            debug!(
                "Loading tileset (index={:?} name={:?}) from {:?}",
                tileset_index, tileset.name, tileset.source
            );
            let mut texture_atlas_layout_handle = None;
            #[cfg(not(feature = "atlas"))]
            let mut tile_image_offsets = HashMap::default();
            let (usable_for_tiles_layer, tilemap_texture) = match &tileset.image {
                None => {
                    #[cfg(feature = "atlas")]
                    {
                        info!("Skipping image collection tileset '{}' which is incompatible with atlas feature", tileset.name);
                        continue;
                    }

                    #[cfg(not(feature = "atlas"))]
                    {
                        let mut usable_for_tiles_layer = true;
                        let mut image_size: Option<(i32, i32)> = None;
                        let mut tile_images: Vec<Handle<Image>> = Vec::new();
                        for (tile_id, tile) in tileset.tiles() {
                            if let Some(img) = &tile.image {
                                let asset_path = AssetPath::from(img.source.clone());
                                debug!("Loading tile image from {asset_path:?} as image ({tileset_index}, {tile_id})");
                                let texture: Handle<Image> = load_context.load(asset_path.clone());
                                tile_image_offsets.insert(tile_id, tile_images.len() as u32);
                                tile_images.push(texture.clone());
                                if usable_for_tiles_layer {
                                    if let Some(image_size) = image_size {
                                        if img.width != image_size.0 || img.height != image_size.1 {
                                            debug!(
                                                "Tileset (index={:?}) have non constant image size and cannot be used for tiles layer",
                                                tileset_index
                                            );
                                            usable_for_tiles_layer = false;
                                        }
                                    } else {
                                        image_size = Some((img.width, img.height));
                                    }
                                }
                            }
                        }
                        (usable_for_tiles_layer, TilemapTexture::Vector(tile_images))
                    }
                }
                Some(img) => {
                    let asset_path = AssetPath::from(img.source.clone());
                    let texture: Handle<Image> = load_context.load(asset_path.clone());

                    let columns = (img.width as u32 - tileset.margin + tileset.spacing)
                        / (tileset.tile_width + tileset.spacing);
                    if columns > 0 {
                        let layout = TextureAtlasLayout::from_grid(
                            UVec2::new(tileset.tile_width, tileset.tile_height),
                            columns,
                            tileset.tilecount / columns,
                            Some(UVec2::new(tileset.spacing, tileset.spacing)),
                            Some(UVec2::new(
                                tileset.offset_x as u32 + tileset.margin,
                                tileset.offset_y as u32 + tileset.margin,
                            )),
                        );
                        texture_atlas_layout_handle = Some(load_context.add_loaded_labeled_asset(
                            tileset.name.clone(),
                            LoadedAsset::from(layout),
                        ));
                    }

                    (true, TilemapTexture::Single(texture.clone()))
                }
            };
            tilesets.insert(
                tileset_index,
                TiledMapTileset {
                    usable_for_tiles_layer,
                    tilemap_texture,
                    texture_atlas_layout_handle,
                    #[cfg(not(feature = "atlas"))]
                    tile_image_offsets,
                },
            );
        }

        #[cfg(feature = "user_properties")]
        let properties =
            DeserializedMapProperties::load(&map, self.registry.read().deref(), load_context);

        #[cfg(feature = "user_properties")]
        trace!(?properties, "user properties");

        let asset_map = TiledMap {
            map,
            tilesets,
            #[cfg(feature = "user_properties")]
            properties,
        };

        debug!("Loaded map '{}'", load_context.path().display());
        Ok(asset_map)
    }

    fn extensions(&self) -> &[&str] {
        static EXTENSIONS: &[&str] = &["tmx"];
        EXTENSIONS
    }
}
