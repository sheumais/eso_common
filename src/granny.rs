//! Port of granny.h: `#[repr(C)]` mirrors of the Granny2 (GR2) structures plus a runtime loader for
//! the exported functions.
//!
//! Notes:
//! * The header's function names had a leading underscore (x86 stdcall decoration, e.g.
//!   `_GrannyReadEntireFile@4`).  Real exports are `GrannyReadEntireFile` etc., which is what is
//!   looked up here.
//! * Many structs in the header were marked "is this correct?" by the original author; they are
//!   reproduced as-is.  Treat them as unverified, especially on 64-bit builds.
//! * Everything here is raw FFI - only use the pointers while the owning `GrannyFile` is alive.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use crate::error::Result;
use crate::eso_error;
use libloading::Library;
use std::ffi::{c_char, c_void, CString};

pub type granny_int8 = i8;
pub type granny_uint8 = u8;
pub type granny_int16 = i16;
pub type granny_uint16 = u16;
pub type granny_int32 = i32;
pub type granny_uint32 = u32;
pub type granny_real32 = f32;

#[repr(C)] #[derive(Clone, Copy)] pub struct Granny_Int16_t { pub Int16: granny_int16 }
#[repr(C)] #[derive(Clone, Copy)] pub struct Granny_Int32_t { pub Int32: granny_int32 }
#[repr(C)] #[derive(Clone, Copy)] pub struct Granny_Real_t { pub Real32: granny_real32 }

/// "is this correct ?" - kept from the header.
#[repr(C)] #[derive(Clone, Copy)]
pub struct granny_transform { pub a: f32, pub b: [f32; 3], pub c: [f32; 4], pub d: [[f32; 3]; 3] }

#[repr(C)]
pub struct StringMember_t {
    pub Type: granny_int32,
    pub Name: *mut c_char,
    pub StructOffset: *mut c_void,
    pub Count: i32,
    pub x: [i32; 4],
}

#[repr(C)]
pub struct ArtToolInfo_t {
    pub FromArtToolName: *mut c_char,
    pub ArtToolMajorRevision: granny_int32,
    pub ArtToolMinorRevision: granny_int32,
    pub UnitsPerMeter: granny_real32,
    pub Origin: [granny_real32; 3],
    pub RightVector: [granny_real32; 3],
    pub UpVector: [granny_real32; 3],
    pub BackVector: [granny_real32; 3],
    pub ExtendedData: *mut c_void,
}

#[repr(C)]
pub struct ExporterInfo_t {
    pub ExporterName: *mut c_char,
    pub ExporterMajorRevision: granny_int32,
    pub ExporterMinorRevision: granny_int32,
    pub ExporterCustomization: granny_int32,
    pub ExporterBuildNumber: granny_int32,
}

#[repr(C)]
pub struct Layout_t {
    pub BytesPerPixel: granny_int32,
    pub ShiftForComponent: [granny_int32; 4],
    pub BitsForComponent: [granny_int32; 4],
}

#[repr(C)] pub struct PixelByte_t { pub UInt8: granny_uint8 }

#[repr(C)]
pub struct MIPLevel_t {
    pub Stride: granny_int32,
    pub nPixelBytes: granny_int32,
    pub PixelBytes: *mut PixelByte_t,
}

#[repr(C)]
pub struct Image_t {
    pub nMIPLevels: granny_int32,
    pub MIPLevels: *mut MIPLevel_t,
}

#[repr(C)]
pub struct Texture_t {
    pub FromFileName: *mut c_char,
    pub TextureType: granny_int32,
    pub Width: granny_int32,
    pub Height: granny_int32,
    pub Encoding: granny_int32,
    pub SubFormat: granny_int32,
    pub Layout: Layout_t,
    pub nImages: granny_int32,
    pub Images: *mut Image_t,
    pub ExtendedData: *mut c_void,
}

#[repr(C)]
pub struct Maps_t {
    pub Usage: *mut c_char,
    pub Map: *mut Material_t,
}

#[repr(C)]
pub struct Material_t {
    pub Name: *mut c_char,
    pub nMaps: granny_int32,
    pub Maps: *mut Maps_t,
    pub Texture: *mut Texture_t,
    pub ExtendedData: *mut c_void,
}

/// "better not use it as it is not known if granny_transform is correct"
#[repr(C)]
pub struct Bone_t {
    pub Name: *mut c_char,
    pub ParentIndex: granny_int32,
    pub Transform: granny_transform,
    pub InverseWorldTransform: [granny_real32; 16],
    pub LODError: granny_real32,
    pub ExtendedData: *mut c_void,
}

#[repr(C)]
pub struct Skeleton_t {
    pub Name: *mut c_char,
    pub nBones: granny_int32,
    pub Bones: *mut Bone_t,
    pub LODType: granny_int32,
}

#[repr(C)] pub struct VertexComponentName_t { pub String: *mut c_char }
pub type VertexAnnotationIndex_t = Granny_Int32_t;

#[repr(C)]
pub struct VertexAnnotationSet_t {
    pub Name: *mut c_char,
    pub nVertexAnnotations: granny_int32,
    pub VertexAnnotations: *mut c_void,
    pub nVertexAnnotationIndices: granny_int32,
    pub VertexAnnotationIndices: *mut VertexAnnotationIndex_t,
}

#[repr(C)]
pub struct VertexData_t {
    pub Vertices_strings: *mut StringMember_t,
    pub nVertices: granny_int32,
    pub Vertices: *mut c_void,
    pub nVertexComponentNames: granny_int32,
    pub VertexComponentNames: *mut VertexComponentName_t,
    pub nVertexAnnotationSets: granny_int32,
    pub VertexAnnotationSets: *mut VertexAnnotationSet_t,
}

#[repr(C)]
pub struct Group_t {
    pub MaterialIndex: granny_int32,
    pub TriFirst: granny_int32,
    pub TriCount: granny_int32,
}

pub type TriAnnotationIndex_t = Granny_Int32_t;

#[repr(C)]
pub struct TriAnnotationSet_t {
    pub Name: *mut c_char,
    pub nTriAnnotations: granny_int32,
    pub TriAnnotations: *mut c_void,
    pub nTriAnnotationIndices: granny_int32,
    pub TriAnnotationIndices: *mut TriAnnotationIndex_t,
}

pub type Index_t = Granny_Int32_t;
pub type Index16_t = Granny_Int16_t;
pub type VertexToVertexMap_t = Granny_Int32_t;
pub type VertexToTriangleMap_t = Granny_Int32_t;
pub type SideToNeighborMap_t = Granny_Int32_t;
pub type BonesForTriangle_t = Granny_Int32_t;
pub type TriangleToBoneIndex_t = Granny_Int32_t;

#[repr(C)]
pub struct TriTopology_t {
    pub nGroups: granny_int32,
    pub Groups: *mut Group_t,
    pub nIndices: granny_int32,
    pub Indices: *mut Index_t,
    pub nIndices16: granny_int32,
    pub Indices16: *mut Index16_t,
    pub nVertexToVertexMap: granny_int32,
    pub VertexToVertexMap: *mut VertexToVertexMap_t,
    pub nVertexToTriangleMap: granny_int32,
    pub VertexToTriangleMap: *mut VertexToTriangleMap_t,
    pub nSideToNeighborMap: granny_int32,
    pub SideToNeighborMap: *mut SideToNeighborMap_t,
    pub nBonesForTriangle: granny_int32,
    pub BonesForTriangle: *mut BonesForTriangle_t,
    pub nTriangleToBoneIndices: granny_int32,
    pub TriangleToBoneIndices: *mut TriangleToBoneIndex_t,
    pub nTriAnnotationSets: granny_int32,
    pub TriAnnotationSets: *mut TriAnnotationSet_t,
}

#[repr(C)]
pub struct MorphTarget_t {
    pub ScalarName: *mut c_char,
    pub VertexData: *mut VertexData_t,
}

#[repr(C)] pub struct MaterialBinding_t { pub Material: *mut Material_t }

pub type TriangleIndex_t = Granny_Int32_t;

#[repr(C)]
pub struct BoneBinding_t {
    pub BoneName: *mut c_char,
    pub OBBMin: [granny_real32; 3],
    pub OBBMax: [granny_real32; 3],
    pub nTriangleIndices: granny_int32,
    pub TriangleIndices: *mut TriangleIndex_t,
}

#[repr(C)]
pub struct Mesh_t {
    pub Name: *mut c_char,
    pub PrimaryVertexData: *mut VertexData_t,
    pub nMorphTargets: granny_int32,
    pub MorphTargets: *mut MorphTarget_t,
    pub PrimaryTopology: *mut TriTopology_t,
    pub nMaterialBindings: granny_int32,
    pub MaterialBindings: *mut MaterialBinding_t,
    pub nBoneBindings: granny_int32,
    pub BoneBindings: *mut BoneBinding_t,
    pub ExtendedData: *mut c_void,
}

#[repr(C)] pub struct MeshBinding_t { pub Mesh: *mut Mesh_t }

#[repr(C)]
pub struct Model_t {
    pub Name: *mut c_char,
    pub Skeleton: *mut Skeleton_t,
    pub InitialPlacement: granny_transform,
    pub nMeshBindings: granny_int32,
    pub MeshBindings: *mut *mut MeshBinding_t,
}

pub type VectorTrack_t = c_void; // unknown

#[repr(C)] pub struct CurveDataHeader_t { pub Format: granny_uint8, pub Degree: granny_uint8 }

pub type Knot_t = Granny_Real_t;
pub type Control_t = Granny_Real_t;

#[repr(C)]
pub struct CurveData_t {
    pub CurveDataHeader_DaK32fC32f_t: CurveDataHeader_t,
    pub Padding: granny_int16,
    pub nKnots: granny_int32,
    pub Knots: *mut Knot_t,
    pub nControls: granny_int32,
    pub Controls: *mut Control_t,
}

#[repr(C)] pub struct Curve_t { pub CurveData: CurveData_t }

pub type OrientationCurve_t = Curve_t;
pub type PositionCurve_t = Curve_t;
pub type ScaleShearCurve_t = Curve_t;

#[repr(C)]
pub struct TransformTrack_t {
    pub Name: *mut c_char,
    pub Flags: granny_int32,
    pub OrientationCurve: OrientationCurve_t,
    pub PositionCurve: PositionCurve_t,
    pub ScaleShearCurve: ScaleShearCurve_t,
}

pub type TransformLODTrack_t = c_void; // unknown

#[repr(C)] pub struct Entry_t { pub TimeStamp: granny_real32, pub Text: *mut c_char }

#[repr(C)]
pub struct TextTrack_t {
    pub Name: *mut c_char,
    pub nEntries: granny_int32,
    pub Entries: *mut Entry_t,
}

#[repr(C)]
pub struct PeriodicLoop_t {
    pub Radius: granny_real32,
    pub dAngle: granny_real32,
    pub dZ: granny_real32,
    pub BasisX: [granny_real32; 3],
    pub BasisY: [granny_real32; 3],
    pub Axis: [granny_real32; 3],
}

pub type RootMotion_t = c_void; // unknown

#[repr(C)]
pub struct TrackGroup_t {
    pub Name: *mut c_char,
    pub nVectorTracks: granny_int32,
    pub VectorTracks: *mut VectorTrack_t,
    pub nTransformTracks: granny_int32,
    pub TransformTracks: *mut TransformTrack_t,
    pub nTransformLODTracks: granny_int32,
    pub TransformLODTracks: *mut TransformLODTrack_t,
    pub nTextTracks: granny_int32,
    pub TextTracks: *mut TextTrack_t,
    pub InitialPlacement: granny_transform,
    pub AccumulationFlags: granny_int32,
    pub LoopTranslation: [granny_real32; 3],
    pub PeriodicLoop: *mut PeriodicLoop_t,
    pub RootMotion: *mut RootMotion_t,
    pub ExtendedData: *mut c_void,
}

#[repr(C)]
pub struct Animation_t {
    pub Name: *mut c_char,
    pub Duration: granny_real32,
    pub TimeStep: granny_real32,
    pub Oversampling: granny_real32,
    pub nTrackGroups: granny_int32,
    pub TrackGroups: *mut *mut TrackGroup_t, // VariantReferenceMember
}

#[repr(C)]
pub struct FileInfo_t {
    pub ArtToolInfo: *mut ArtToolInfo_t,
    pub ExporterInfo: *mut ExporterInfo_t,
    pub FromFileName: *mut c_char,
    pub nTextures: granny_int32,
    pub Textures: *mut *mut Texture_t,
    pub nMaterials: granny_int32,
    pub Materials: *mut *mut Material_t,
    pub nSkeletons: granny_int32,
    pub Skeletons: *mut *mut Skeleton_t,
    pub nVertexDatas: granny_int32,
    pub VertexDatas: *mut *mut VertexData_t,
    pub nTriTopologies: granny_int32,
    pub TriTopologies: *mut *mut TriTopology_t,
    pub nMeshes: granny_int32,
    pub Meshes: *mut *mut Mesh_t,
    pub nModels: granny_int32,
    pub Models: *mut *mut Model_t,
    pub nTrackGroups: granny_int32,
    pub TrackGroups: *mut *mut TrackGroup_t,
    pub nAnimations: granny_int32,
    pub Animations: *mut *mut Animation_t,
}

/// "how is this structure" - layout is a guess in the original.
#[repr(C)]
pub struct GrannyFile_t {
    pub unknown01: [c_char; 4],
    pub filerevision: *mut granny_uint32,
    pub unknown012: *mut c_void,
    pub sections: granny_uint32,
    pub uknown02: *mut c_void,
    pub uknown03: *mut c_void,
    pub uknown04: *mut c_void,
    pub fileInfo: *mut FileInfo_t,
    pub unknownFileHeader: [c_char; 32],
    pub unknown05: *mut *mut ArtToolInfo_t,
    pub unknown: [c_char; 28],
}

#[repr(C)]
pub struct GrannyLogger {
    pub a: Option<unsafe extern "C" fn(a: i32, b: i32, msg: *mut c_char)>,
    pub b: Option<unsafe extern "C" fn(a: i32, b: i32, msg: *mut c_char)>,
}

// ---------------------------------------------------------------------------------------------
// Dynamic API
// ---------------------------------------------------------------------------------------------

type Sys<T> = unsafe extern "system" fn(T) -> granny_int32;

/// Function table loaded from the Granny2 DLL (e.g. `granny2.dll` / `granny2_x64.dll`).
pub struct GrannyApi {
    _lib: Library,
    pub copy_mesh_indices: unsafe extern "system" fn(*const Mesh_t, granny_int32, *mut c_void) -> granny_int32,
    pub copy_mesh_vertices: unsafe extern "system" fn(*const Mesh_t, granny_int32, *mut c_void) -> granny_int32,
    pub convert_file_to_raw: unsafe extern "system" fn(*const c_char, *const c_char) -> granny_int32,
    pub free_file: Sys<*mut GrannyFile_t>,
    pub get_file_info: unsafe extern "system" fn(*const GrannyFile_t) -> *mut FileInfo_t,
    pub get_file_type_tag: Sys<*const GrannyFile_t>,
    pub get_mesh_bytes_per_index: Sys<*const Mesh_t>,
    pub get_mesh_index_count: Sys<*const Mesh_t>,
    pub get_mesh_indices: unsafe extern "system" fn(*const Mesh_t) -> *mut c_void,
    pub get_mesh_triangle_count: Sys<*const Mesh_t>,
    pub get_mesh_triangle_group_count: Sys<*const Mesh_t>,
    pub get_mesh_triangle_groups: unsafe extern "system" fn(*const Mesh_t) -> *mut Group_t,
    pub get_mesh_vertex_count: Sys<*const Mesh_t>,
    pub get_mesh_vertex_type: unsafe extern "system" fn(*const Mesh_t) -> *mut granny_int32,
    pub get_mesh_vertices: unsafe extern "system" fn(*const Mesh_t) -> *mut c_void,
    pub get_vertex_component_count: Sys<*const StringMember_t>,
    /// Returns -1 if not found.
    pub get_vertex_component_index: unsafe extern "system" fn(*const c_char, *const StringMember_t) -> granny_int32,
    pub get_version_string: unsafe extern "system" fn() -> *const c_char,
    pub read_entire_file: unsafe extern "system" fn(*const c_char) -> *mut GrannyFile_t,
    pub read_entire_file_from_memory: unsafe extern "system" fn(granny_int32, *const c_void) -> *mut GrannyFile_t,
    pub set_log_callback: unsafe extern "system" fn(*mut GrannyLogger) -> granny_int32,
}

macro_rules! sym {
    ($lib:expr, $name:literal) => {
        // SAFETY: the declared field type is the signature from granny.h.
        *unsafe { $lib.get(concat!($name, "\0").as_bytes()) }
            .map_err(|e| format!("Error: Granny function '{}' not found: {}", $name, e))?
    };
}

impl GrannyApi {
    /// Loads the library, logging an error on failure.
    pub fn load(dll_path: &str) -> Result<Self> {
        Self::try_load(dll_path).map_err(|m| eso_error!("{}", m))
    }

    /// Loads the library without logging.
    pub fn try_load(dll_path: &str) -> std::result::Result<Self, String> {
        // SAFETY: loading a shared library runs its initialisers.
        let lib = unsafe { Library::new(dll_path) }
            .map_err(|e| format!("Error: Failed to load the Granny library '{}': {}", dll_path, e))?;
        Ok(Self {
            copy_mesh_indices: sym!(lib, "GrannyCopyMeshIndices"),
            copy_mesh_vertices: sym!(lib, "GrannyCopyMeshVertices"),
            convert_file_to_raw: sym!(lib, "GrannyConvertFileToRaw"),
            free_file: sym!(lib, "GrannyFreeFile"),
            get_file_info: sym!(lib, "GrannyGetFileInfo"),
            get_file_type_tag: sym!(lib, "GrannyGetFileTypeTag"),
            get_mesh_bytes_per_index: sym!(lib, "GrannyGetMeshBytesPerIndex"),
            get_mesh_index_count: sym!(lib, "GrannyGetMeshIndexCount"),
            get_mesh_indices: sym!(lib, "GrannyGetMeshIndices"),
            get_mesh_triangle_count: sym!(lib, "GrannyGetMeshTriangleCount"),
            get_mesh_triangle_group_count: sym!(lib, "GrannyGetMeshTriangleGroupCount"),
            get_mesh_triangle_groups: sym!(lib, "GrannyGetMeshTriangleGroups"),
            get_mesh_vertex_count: sym!(lib, "GrannyGetMeshVertexCount"),
            get_mesh_vertex_type: sym!(lib, "GrannyGetMeshVertexType"),
            get_mesh_vertices: sym!(lib, "GrannyGetMeshVertices"),
            get_vertex_component_count: sym!(lib, "GrannyGetVertexComponentCount"),
            get_vertex_component_index: sym!(lib, "GrannyGetVertexComponentIndex"),
            get_version_string: sym!(lib, "GrannyGetVersionString"),
            read_entire_file: sym!(lib, "GrannyReadEntireFile"),
            read_entire_file_from_memory: sym!(lib, "GrannyReadEntireFileFromMemory"),
            set_log_callback: sym!(lib, "GrannySetLogCallback"),
            _lib: lib,
        })
    }

    /// Parses a GR2 file held in memory.  The returned handle frees the file on drop.
    pub fn read_from_memory(&self, data: &[u8]) -> Result<GrannyFileHandle<'_>> {
        // SAFETY: pointer/length come from a valid slice.
        let p = unsafe { (self.read_entire_file_from_memory)(data.len() as granny_int32, data.as_ptr().cast()) };
        if p.is_null() { return Err(eso_error!("Error: Granny failed to parse the GR2 data!")); }
        Ok(GrannyFileHandle { api: self, file: p })
    }

    pub fn read_file(&self, filename: &str) -> Result<GrannyFileHandle<'_>> {
        let c = CString::new(filename).map_err(|_| eso_error!("Error: Invalid filename '{}'!", filename))?;
        // SAFETY: `c` is a valid NUL terminated string.
        let p = unsafe { (self.read_entire_file)(c.as_ptr()) };
        if p.is_null() { return Err(eso_error!("Error: Granny failed to read '{}'!", filename)); }
        Ok(GrannyFileHandle { api: self, file: p })
    }
}

pub struct GrannyFileHandle<'a> {
    api: &'a GrannyApi,
    file: *mut GrannyFile_t,
}

impl GrannyFileHandle<'_> {
    /// Pointer to the file info; valid until the handle is dropped.  May be null.
    pub fn file_info(&self) -> *mut FileInfo_t {
        // SAFETY: `self.file` is non-null and owned by this handle.
        unsafe { (self.api.get_file_info)(self.file) }
    }

    pub fn raw(&self) -> *mut GrannyFile_t { self.file }
}

impl Drop for GrannyFileHandle<'_> {
    fn drop(&mut self) {
        // SAFETY: `self.file` came from GrannyReadEntireFile* and is freed exactly once.
        unsafe { (self.api.free_file)(self.file); }
    }
}

/// Process-wide Granny API, loaded on first use from the usual DLL names (`granny2.dll`, ...).
/// `None` if no library could be loaded (GR2 files are then saved without name parsing).
pub fn global_api() -> Option<&'static GrannyApi> {
    use std::sync::OnceLock;
    static API: OnceLock<Option<GrannyApi>> = OnceLock::new();
    API.get_or_init(|| {
        ["granny2.dll", "granny2_x64.dll", "libgranny2.so"]
            .iter()
            .find_map(|n| GrannyApi::try_load(n).ok())
    })
    .as_ref()
}

/// Copies a C string (null -> empty).
///
/// # Safety
/// `p` must be null or point to a valid NUL-terminated string.
pub unsafe fn cstr_lossy(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned() }
}
