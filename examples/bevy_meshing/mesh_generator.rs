use building_blocks::core::prelude::*;
use building_blocks::mesh::{height_map::*, surface_nets::*, PosNormMesh};
use building_blocks::procgen::signed_distance_fields::*;
use building_blocks::storage::prelude::*;

use bevy::{
    prelude::*,
    render::{mesh::VertexAttribute, pipeline::PrimitiveTopology},
};

pub struct MeshGeneratorState {
    current_shape_index: i32,
    chunk_mesh_entities: Vec<Entity>,

    // reused to avoid reallocations
    surface_nets_buffer: SurfaceNetsBuffer,
    height_map_mesh_buffer: HeightMapMeshBuffer,
}

impl MeshGeneratorState {
    pub fn new() -> Self {
        Self {
            current_shape_index: 0,
            height_map_mesh_buffer: HeightMapMeshBuffer::default(),
            surface_nets_buffer: SurfaceNetsBuffer::default(),
            chunk_mesh_entities: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Shape {
    Sdf(Sdf),
    HeightMap(HeightMap),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Sdf {
    Cube,
    Plane,
    Sphere,
    Torus,
}

impl Sdf {
    fn get_sdf(&self) -> Box<dyn Fn(&Point3i) -> f32> {
        match self {
            Sdf::Cube => Box::new(cube(PointN([0.0, 0.0, 0.0]), 35.0)),
            Sdf::Plane => Box::new(plane(PointN([0.5, 0.5, 0.5]), 1.0)),
            Sdf::Sphere => Box::new(sphere(PointN([0.0, 0.0, 0.0]), 35.0)),
            Sdf::Torus => Box::new(torus(PointN([35.0, 10.0]))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HeightMap {
    Wave,
}

impl HeightMap {
    fn get_height_map(&self) -> impl Fn(&Point2i) -> f32 {
        match self {
            HeightMap::Wave => {
                |p: &Point2i| 10.0 * (1.0 + (0.1 * p.x() as f32).cos() + (0.1 * p.y() as f32).sin())
            }
        }
    }
}

const NUM_SHAPES: i32 = 5;

fn choose_shape(index: i32) -> Shape {
    match index {
        0 => Shape::Sdf(Sdf::Cube),
        1 => Shape::Sdf(Sdf::Plane),
        2 => Shape::Sdf(Sdf::Sphere),
        3 => Shape::Sdf(Sdf::Torus),
        4 => Shape::HeightMap(HeightMap::Wave),
        _ => panic!("bad shape index"),
    }
}

#[derive(Default)]
pub struct MeshMaterial(pub Handle<StandardMaterial>);

pub fn mesh_generator_system(
    mut commands: Commands,
    mut state: ResMut<MeshGeneratorState>,
    mut meshes: ResMut<Assets<Mesh>>,
    keyboard_input: Res<Input<KeyCode>>,
    material: Res<MeshMaterial>,
) {
    let mut new_shape_requested = false;
    if keyboard_input.just_pressed(KeyCode::Left) {
        new_shape_requested = true;
        state.current_shape_index = (state.current_shape_index - 1).rem_euclid(NUM_SHAPES);
    } else if keyboard_input.just_pressed(KeyCode::Right) {
        new_shape_requested = true;
        state.current_shape_index = (state.current_shape_index + 1).rem_euclid(NUM_SHAPES);
    }

    if new_shape_requested || state.chunk_mesh_entities.is_empty() {
        // Delete the old meshes.
        for entity in state.chunk_mesh_entities.drain(..) {
            commands.despawn(entity);
        }

        // Sample the new shape SDF in some extent.
        let shape = choose_shape(state.current_shape_index);
        match shape {
            Shape::Sdf(sdf) => generate_chunk_meshes_from_sdf(
                sdf,
                &mut state,
                &mut commands,
                material.0,
                &mut meshes,
            ),
            Shape::HeightMap(hm) => generate_chunk_meshes_from_height_map(
                hm,
                &mut state,
                &mut commands,
                material.0,
                &mut meshes,
            ),
        }
    }
}

fn generate_chunk_meshes_from_sdf(
    sdf: Sdf,
    state: &mut MeshGeneratorState,
    commands: &mut Commands,
    material: Handle<StandardMaterial>,
    meshes: &mut Assets<Mesh>,
) {
    let sdf = sdf.get_sdf();
    let sample_extent = Extent3i::from_min_and_shape(PointN([-50; 3]), PointN([100; 3]));
    let chunk_shape = PointN([16; 3]);
    let ambient_value = std::f32::MAX; // air
    let default_chunk_meta = ();
    // Normally we'd keep this map around in a resource, but we don't need to for this specific
    // example. We could also use an Array3 here instead of a ChunkMap3, but we use chunks for
    // educational purposes.
    let mut map = ChunkMap3::new(
        chunk_shape,
        ambient_value,
        default_chunk_meta,
        FastLz4 { level: 10 },
    );
    copy_extent(&sample_extent, &sdf, &mut map);

    // Generate the chunk meshes.
    let local_cache = LocalChunkCache::new();
    let map_reader = ChunkMapReader3::new(&map, &local_cache);
    for chunk_key in map.chunk_keys() {
        let padded_chunk_extent = map.extent_for_chunk_at_key(chunk_key).padded(1);
        let mut padded_chunk = Array3::fill(padded_chunk_extent, 0.0);
        copy_extent(&padded_chunk_extent, &map_reader, &mut padded_chunk);
        surface_nets(
            &padded_chunk,
            &padded_chunk_extent,
            &mut state.surface_nets_buffer,
        );

        if state.surface_nets_buffer.mesh.indices.is_empty() {
            continue;
        }

        state.chunk_mesh_entities.push(create_mesh_entity(
            &state.surface_nets_buffer.mesh,
            commands,
            material,
            meshes,
        ));
    }
}

fn generate_chunk_meshes_from_height_map(
    hm: HeightMap,
    state: &mut MeshGeneratorState,
    commands: &mut Commands,
    material: Handle<StandardMaterial>,
    meshes: &mut Assets<Mesh>,
) {
    let height_map = hm.get_height_map();
    let sample_extent = Extent2i::from_min_and_shape(PointN([-50; 2]), PointN([100; 2]));
    let chunk_shape = PointN([16; 2]);
    let ambient_value = 0.0;
    let default_chunk_meta = ();
    // Normally we'd keep this map around in a resource, but we don't need to for this specific
    // example. We could also use an Array3 here instead of a ChunkMap3, but we use chunks for
    // educational purposes.
    let mut map = ChunkMap2::new(
        chunk_shape,
        ambient_value,
        default_chunk_meta,
        FastLz4 { level: 10 },
    );
    copy_extent(&sample_extent, &height_map, &mut map);

    // Generate the chunk meshes.
    let local_cache = LocalChunkCache::new();
    let map_reader = ChunkMapReader2::new(&map, &local_cache);
    for chunk_key in map.chunk_keys() {
        // TODO: this padding is kind of odd, but it's exactly what's required for the height map
        // meshing scheme
        let padded_chunk_extent = map
            .extent_for_chunk_at_key(chunk_key)
            .padded(1)
            .add_to_shape(PointN([1; 2]))
            // Ignore the ambient values outside the sample extent.
            .intersection(&sample_extent);

        let mut padded_chunk = Array2::fill(padded_chunk_extent, 0.0);
        copy_extent(&padded_chunk_extent, &map_reader, &mut padded_chunk);
        triangulate_height_map(
            &padded_chunk,
            &padded_chunk_extent,
            &mut state.height_map_mesh_buffer,
        );

        if state.height_map_mesh_buffer.mesh.indices.is_empty() {
            continue;
        }

        state.chunk_mesh_entities.push(create_mesh_entity(
            &state.height_map_mesh_buffer.mesh,
            commands,
            material,
            meshes,
        ));
    }
}

fn create_mesh_entity(
    mesh: &PosNormMesh,
    commands: &mut Commands,
    material: Handle<StandardMaterial>,
    meshes: &mut Assets<Mesh>,
) -> Entity {
    let mesh = meshes.add(Mesh {
        primitive_topology: PrimitiveTopology::TriangleList,
        attributes: vec![
            VertexAttribute::position(mesh.positions.clone()),
            VertexAttribute::normal(mesh.normals.clone()),
            // UVs don't matter for this monocolor mesh
            VertexAttribute::uv(vec![[0.0; 2]; mesh.normals.len()]),
        ],
        indices: Some(mesh.indices.iter().map(|i| *i as u32).collect()),
    });

    commands
        .spawn(PbrComponents {
            mesh,
            material,
            ..Default::default()
        })
        .current_entity()
        .unwrap()
}