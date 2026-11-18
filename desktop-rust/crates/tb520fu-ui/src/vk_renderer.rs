// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use ash::vk;

use crate::canvas::{AtlasBank, AtlasTarget, SdfInstance};
use crate::color::Rgba;
use crate::wayland::SurfaceId;
use crate::widgets::BitmapIcon;

const UBER_VS_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/uber_sdf_vs.spv"));
const UBER_FS_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/uber_sdf_fs.spv"));
const KAWASE_VS_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kawase_vs.spv"));
const KAWASE_DOWN_FS_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kawase_down_fs.spv"));
const KAWASE_UP_FS_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kawase_up_fs.spv"));

pub const ATLAS_DIM: u32 = 2048;
const MAX_INSTANCES: usize = 32_768;
const WSI_RING: usize = 4;
const RING_MAX_INSTANCES: usize = 8_192;
const STAGING_CAP: usize = 32 * 1024 * 1024; // 32 MiB shared upload/readback ring

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct GlobalsUniform {
    target_size: [f32; 2],
    wallpaper_size: [f32; 2],
    surf_origin: [f32; 2],
    extra: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BlurUniform {
    texel: [f32; 2],
    offset: f32,
    sat_boost: f32,
}

#[allow(dead_code)]
struct GpuImage {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    width: u32,
    height: u32,
    format: vk::Format,
    layout: vk::ImageLayout,
}

#[allow(dead_code)]
struct GpuBuffer {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    mapped: *mut u8,
    size: usize,
}

unsafe impl Send for GpuBuffer {}

#[allow(dead_code)]
struct SwapchainSurface {
    surface_khr: vk::SurfaceKHR,
    swapchain: vk::SwapchainKHR,
    format: vk::Format,
    extent: vk::Extent2D,
    images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
    framebuffers: Vec<vk::Framebuffer>,
    render_pass: vk::RenderPass,
    pipeline: vk::Pipeline,
    image_avail: vk::Semaphore,
    render_done: vk::Semaphore,
}

#[allow(dead_code)]
struct WsiFrameSlot {
    cmd_buf: vk::CommandBuffer,
    fence: vk::Fence,
    globals_buf: GpuBuffer,
    instance_buf: GpuBuffer,
    desc_set: vk::DescriptorSet,
}

struct PendingCachePass {
    phys_w: u32,
    phys_h: u32,
    clear: Rgba,
    surf_origin: [f32; 2],
    wallpaper_phys: [f32; 2],
    instances: Vec<SdfInstance>,
}

#[allow(dead_code)]
pub struct VkRenderer {
    entry: ash::Entry,
    instance: ash::Instance,
    surface_ext: Option<ash::khr::surface::Instance>,
    wl_surface_ext: Option<ash::khr::wayland_surface::Instance>,
    swapchain_ext: Option<ash::khr::swapchain::Device>,
    phys_dev: vk::PhysicalDevice,
    pub device_name: String,
    pub is_hardware_gpu: bool,
    device: ash::Device,
    queue_family: u32,
    queue: vk::Queue,
    cmd_pool: vk::CommandPool,
    cmd_buf: vk::CommandBuffer,
    fence: vk::Fence,
    wsi_ring: Vec<WsiFrameSlot>,
    wsi_ring_cursor: usize,

    sampler_linear: vk::Sampler,
    uber_vs: vk::ShaderModule,
    uber_fs: vk::ShaderModule,
    uber_dsl: vk::DescriptorSetLayout,
    uber_pipe_layout: vk::PipelineLayout,
    desc_pool: vk::DescriptorPool,
    uber_desc_set: vk::DescriptorSet,

    offscreen_rp: vk::RenderPass,
    offscreen_pipe: vk::Pipeline,
    offscreen_target: Option<(GpuImage, vk::Framebuffer)>,

    cache_rp: vk::RenderPass,
    cache_pipe: vk::Pipeline,
    tex_cache: GpuImage,
    cache_fb: vk::Framebuffer,
    cache_valid_key: u64,
    pending_cache: Option<PendingCachePass>,

    globals_buf: GpuBuffer,
    instance_buf: GpuBuffer,
    staging_buf: GpuBuffer,

    tex_r8: GpuImage,
    tex_rgba: GpuImage,
    tex_wallpaper: GpuImage,
    tex_glass: GpuImage,
    tex_photo: GpuImage,
    photo_uploaded_id: u64,
    atlas_epoch_seen: u64,
    wallpaper_phys_size: [f32; 2],

    // Dual-Kawase blur resources
    kawase_dsl: vk::DescriptorSetLayout,
    kawase_pipe_layout: vk::PipelineLayout,
    kawase_rp: vk::RenderPass,
    kawase_down_pipe: vk::Pipeline,
    kawase_up_pipe: vk::Pipeline,
    kawase_ubos: Vec<GpuBuffer>,
    kawase_sets: Vec<vk::DescriptorSet>,

    wsi_surfaces: HashMap<SurfaceId, SwapchainSurface>,
    /// Render pass + pipeline for presenting, keyed by swapchain format. Built once and shared by
    /// every swapchain: compiling the pipeline is the slow part of (re)creating a swapchain.
    wsi_pipes: HashMap<vk::Format, (vk::RenderPass, vk::Pipeline)>,
}

unsafe impl Send for VkRenderer {}

static GLOBAL_VK: OnceLock<Option<Mutex<VkRenderer>>> = OnceLock::new();

impl VkRenderer {
    pub fn global() -> Option<&'static Mutex<Self>> {
        GLOBAL_VK
            .get_or_init(|| match Self::init() {
                Ok(vk) => {
                    eprintln!(
                        "tb520fu-ui: Vulkan engine active on '{}' (hw_gpu={})",
                        vk.device_name, vk.is_hardware_gpu
                    );
                    Some(Mutex::new(vk))
                }
                Err(e) => {
                    eprintln!("tb520fu-ui: Vulkan init unavailable ({e}), using CPU fallback");
                    None
                }
            })
            .as_ref()
    }

    fn bytes_to_spv_words(bytes: &[u8]) -> Vec<u32> {
        bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    pub fn init() -> Result<Self, String> {
        unsafe {
            let entry = ash::Entry::load().map_err(|e| format!("load libvulkan.so.1: {e}"))?;
            let app_info = vk::ApplicationInfo::default()
                .application_name(c"tb520fu-desktop")
                .application_version(vk::make_api_version(0, 0, 2, 0))
                .engine_name(c"tb520fu-uber-sdf")
                .api_version(vk::API_VERSION_1_1);

            let avail_inst_exts = entry
                .enumerate_instance_extension_properties(None)
                .unwrap_or_default();
            let has_ext = |name: &CStr| {
                avail_inst_exts
                    .iter()
                    .any(|p| CStr::from_ptr(p.extension_name.as_ptr()) == name)
            };
            let mut inst_exts: Vec<*const std::ffi::c_char> = Vec::new();
            let wsi_supported = has_ext(ash::khr::surface::NAME)
                && has_ext(ash::khr::wayland_surface::NAME);
            if wsi_supported {
                inst_exts.push(ash::khr::surface::NAME.as_ptr());
                inst_exts.push(ash::khr::wayland_surface::NAME.as_ptr());
            }

            let inst_ci = vk::InstanceCreateInfo::default()
                .application_info(&app_info)
                .enabled_extension_names(&inst_exts);
            let instance = entry
                .create_instance(&inst_ci, None)
                .map_err(|e| format!("vkCreateInstance: {e}"))?;

            let surface_ext = wsi_supported.then(|| ash::khr::surface::Instance::new(&entry, &instance));
            let wl_surface_ext =
                wsi_supported.then(|| ash::khr::wayland_surface::Instance::new(&entry, &instance));

            let pdevs = instance
                .enumerate_physical_devices()
                .map_err(|e| format!("enumerate_physical_devices: {e}"))?;
            if pdevs.is_empty() {
                return Err("no Vulkan physical devices found".into());
            }

            // Strictly prefer Turnip Adreno 750 (`INTEGRATED_GPU`) -> `DISCRETE_GPU` -> `VIRTUAL_GPU` -> `CPU`
            let mut best: Option<(i32, vk::PhysicalDevice, u32, String, bool)> = None;
            for pd in pdevs {
                let props = instance.get_physical_device_properties(pd);
                let qprops = instance.get_physical_device_queue_family_properties(pd);
                let Some(qidx) = qprops
                    .iter()
                    .position(|q| q.queue_flags.contains(vk::QueueFlags::GRAPHICS))
                else {
                    continue;
                };
                let name = CStr::from_ptr(props.device_name.as_ptr())
                    .to_string_lossy()
                    .into_owned();
                let (score, hw) = match props.device_type {
                    vk::PhysicalDeviceType::INTEGRATED_GPU => (100, true),
                    vk::PhysicalDeviceType::DISCRETE_GPU => (90, true),
                    vk::PhysicalDeviceType::VIRTUAL_GPU => (50, true),
                    vk::PhysicalDeviceType::OTHER => (25, false),
                    _ => (10, false),
                };
                if best.as_ref().is_none_or(|b| score > b.0) {
                    best = Some((score, pd, qidx as u32, name, hw));
                }
            }
            let (_, phys_dev, queue_family, device_name, is_hardware_gpu) =
                best.ok_or("no graphics-capable Vulkan queue family")?;

            let dev_exts_avail = instance
                .enumerate_device_extension_properties(phys_dev)
                .unwrap_or_default();
            let has_swapchain = wsi_supported
                && dev_exts_avail
                    .iter()
                    .any(|p| CStr::from_ptr(p.extension_name.as_ptr()) == ash::khr::swapchain::NAME);
            let mut dev_exts: Vec<*const std::ffi::c_char> = Vec::new();
            if has_swapchain {
                dev_exts.push(ash::khr::swapchain::NAME.as_ptr());
            }

            let prio = [1.0_f32];
            let qci = [vk::DeviceQueueCreateInfo::default()
                .queue_family_index(queue_family)
                .queue_priorities(&prio)];
            let dev_ci = vk::DeviceCreateInfo::default()
                .queue_create_infos(&qci)
                .enabled_extension_names(&dev_exts);
            let device = instance
                .create_device(phys_dev, &dev_ci, None)
                .map_err(|e| format!("vkCreateDevice: {e}"))?;
            let swapchain_ext = has_swapchain.then(|| ash::khr::swapchain::Device::new(&instance, &device));
            let queue = device.get_device_queue(queue_family, 0);

            let pool_ci = vk::CommandPoolCreateInfo::default()
                .queue_family_index(queue_family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
            let cmd_pool = device
                .create_command_pool(&pool_ci, None)
                .map_err(|e| e.to_string())?;
            let alloc_ci = vk::CommandBufferAllocateInfo::default()
                .command_pool(cmd_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1);
            let cmd_buf = device
                .allocate_command_buffers(&alloc_ci)
                .map_err(|e| e.to_string())?[0];
            let fence = device
                .create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )
                .map_err(|e| e.to_string())?;

            let sampler_linear = device
                .create_sampler(
                    &vk::SamplerCreateInfo::default()
                        .mag_filter(vk::Filter::LINEAR)
                        .min_filter(vk::Filter::LINEAR)
                        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                    None,
                )
                .map_err(|e| e.to_string())?;

            let mk_shader = |bytes: &[u8]| -> Result<vk::ShaderModule, String> {
                let words = Self::bytes_to_spv_words(bytes);
                device
                    .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)
                    .map_err(|e| format!("vkCreateShaderModule: {e}"))
            };
            let uber_vs = mk_shader(UBER_VS_SPV)?;
            let uber_fs = mk_shader(UBER_FS_SPV)?;
            let kawase_vs = mk_shader(KAWASE_VS_SPV)?;
            let kawase_down_fs = mk_shader(KAWASE_DOWN_FS_SPV)?;
            let kawase_up_fs = mk_shader(KAWASE_UP_FS_SPV)?;

            // Uber-SDF DescriptorSetLayout (bindings 0..=7)
            let uber_bindings = [
                vk::DescriptorSetLayoutBinding::default()
                    .binding(0)
                    .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(1)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(2)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(3)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(4)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(5)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(6)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(7)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
            ];
            let uber_dsl = device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&uber_bindings),
                    None,
                )
                .map_err(|e| e.to_string())?;
            let uber_pipe_layout = device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default()
                        .set_layouts(std::slice::from_ref(&uber_dsl)),
                    None,
                )
                .map_err(|e| e.to_string())?;

            // Kawase Blur DescriptorSetLayout (bindings 0..=2)
            let kawase_bindings = [
                vk::DescriptorSetLayoutBinding::default()
                    .binding(0)
                    .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(1)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(2)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::FRAGMENT),
            ];
            let kawase_dsl = device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&kawase_bindings),
                    None,
                )
                .map_err(|e| e.to_string())?;
            let kawase_pipe_layout = device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default()
                        .set_layouts(std::slice::from_ref(&kawase_dsl)),
                    None,
                )
                .map_err(|e| e.to_string())?;

            let pool_sizes = [
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::UNIFORM_BUFFER,
                    descriptor_count: 24,
                },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::STORAGE_BUFFER,
                    descriptor_count: 12,
                },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::SAMPLER,
                    descriptor_count: 24,
                },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::SAMPLED_IMAGE,
                    descriptor_count: 64,
                },
            ];
            let desc_pool = device
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .max_sets(24)
                        .pool_sizes(&pool_sizes),
                    None,
                )
                .map_err(|e| e.to_string())?;

            let uber_desc_set = device
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(desc_pool)
                        .set_layouts(std::slice::from_ref(&uber_dsl)),
                )
                .map_err(|e| e.to_string())?[0];
            let ring_uber_layouts = [uber_dsl; WSI_RING];
            let ring_desc_sets = device
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(desc_pool)
                        .set_layouts(&ring_uber_layouts),
                )
                .map_err(|e| e.to_string())?;
            let ring_cmd_bufs = device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(cmd_pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(WSI_RING as u32),
                )
                .map_err(|e| e.to_string())?;

            let mut kawase_layouts = [kawase_dsl; 8];
            let kawase_sets = device
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(desc_pool)
                        .set_layouts(&kawase_layouts),
                )
                .map_err(|e| e.to_string())?;
            let _ = &mut kawase_layouts;

            let offscreen_rp = Self::create_render_pass(
                &device,
                vk::Format::R8G8B8A8_UNORM,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                true,
            )?;
            let offscreen_pipe = Self::create_uber_pipeline(
                &device,
                uber_pipe_layout,
                offscreen_rp,
                uber_vs,
                uber_fs,
                true,
            )?;

            let cache_rp = Self::create_render_pass(
                &device,
                vk::Format::R8G8B8A8_UNORM,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                true,
            )?;
            let cache_pipe = Self::create_uber_pipeline(
                &device,
                uber_pipe_layout,
                cache_rp,
                uber_vs,
                uber_fs,
                true,
            )?;

            let kawase_rp = Self::create_render_pass(
                &device,
                vk::Format::R8G8B8A8_UNORM,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                false,
            )?;
            let kawase_down_pipe = Self::create_kawase_pipeline_named(
                &device,
                kawase_pipe_layout,
                kawase_rp,
                kawase_vs,
                kawase_down_fs,
                false,
            )?;
            let kawase_up_pipe = Self::create_kawase_pipeline_named(
                &device,
                kawase_pipe_layout,
                kawase_rp,
                kawase_vs,
                kawase_up_fs,
                true,
            )?;

            device.destroy_shader_module(kawase_vs, None);
            device.destroy_shader_module(kawase_down_fs, None);
            device.destroy_shader_module(kawase_up_fs, None);

            let mem_props = instance.get_physical_device_memory_properties(phys_dev);
            let globals_buf = Self::alloc_host_buffer(
                &device,
                &mem_props,
                std::mem::size_of::<GlobalsUniform>(),
                vk::BufferUsageFlags::UNIFORM_BUFFER,
            )?;
            let instance_buf = Self::alloc_host_buffer(
                &device,
                &mem_props,
                MAX_INSTANCES * std::mem::size_of::<SdfInstance>(),
                vk::BufferUsageFlags::STORAGE_BUFFER,
            )?;
            let staging_buf = Self::alloc_host_buffer(
                &device,
                &mem_props,
                STAGING_CAP,
                vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST,
            )?;

            let mut wsi_ring = Vec::with_capacity(WSI_RING);
            for i in 0..WSI_RING {
                let rfence = device
                    .create_fence(
                        &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                        None,
                    )
                    .map_err(|e| e.to_string())?;
                let rglobals = Self::alloc_host_buffer(
                    &device,
                    &mem_props,
                    std::mem::size_of::<GlobalsUniform>(),
                    vk::BufferUsageFlags::UNIFORM_BUFFER,
                )?;
                let rinstances = Self::alloc_host_buffer(
                    &device,
                    &mem_props,
                    RING_MAX_INSTANCES * std::mem::size_of::<SdfInstance>(),
                    vk::BufferUsageFlags::STORAGE_BUFFER,
                )?;
                wsi_ring.push(WsiFrameSlot {
                    cmd_buf: ring_cmd_bufs[i],
                    fence: rfence,
                    globals_buf: rglobals,
                    instance_buf: rinstances,
                    desc_set: ring_desc_sets[i],
                });
            }

            let mut kawase_ubos = Vec::with_capacity(8);
            for _ in 0..8 {
                kawase_ubos.push(Self::alloc_host_buffer(
                    &device,
                    &mem_props,
                    std::mem::size_of::<BlurUniform>(),
                    vk::BufferUsageFlags::UNIFORM_BUFFER,
                )?);
            }

            let tex_r8 = Self::alloc_image(
                &device,
                &mem_props,
                ATLAS_DIM,
                ATLAS_DIM,
                vk::Format::R8_UNORM,
                vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
            )?;
            let tex_rgba = Self::alloc_image(
                &device,
                &mem_props,
                ATLAS_DIM,
                ATLAS_DIM,
                vk::Format::R8G8B8A8_UNORM,
                vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
            )?;
            let tex_wallpaper = Self::alloc_image(
                &device,
                &mem_props,
                1472,
                920,
                vk::Format::R8G8B8A8_UNORM,
                vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
            )?;
            let tex_glass = Self::alloc_image(
                &device,
                &mem_props,
                736,
                460,
                vk::Format::R8G8B8A8_UNORM,
                vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::COLOR_ATTACHMENT,
            )?;
            let tex_photo = Self::alloc_image(
                &device,
                &mem_props,
                64,
                64,
                vk::Format::R8G8B8A8_UNORM,
                vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
            )?;
            let tex_cache = Self::alloc_image(
                &device,
                &mem_props,
                64,
                64,
                vk::Format::R8G8B8A8_UNORM,
                vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::COLOR_ATTACHMENT
                    | vk::ImageUsageFlags::TRANSFER_DST,
            )?;
            let cache_fb = device
                .create_framebuffer(
                    &vk::FramebufferCreateInfo::default()
                        .render_pass(cache_rp)
                        .attachments(std::slice::from_ref(&tex_cache.view))
                        .width(64)
                        .height(64)
                        .layers(1),
                    None,
                )
                .map_err(|e| e.to_string())?;

            let mut renderer = Self {
                entry,
                instance,
                surface_ext,
                wl_surface_ext,
                swapchain_ext,
                phys_dev,
                device_name,
                is_hardware_gpu,
                device,
                queue_family,
                queue,
                cmd_pool,
                cmd_buf,
                fence,
                wsi_ring,
                wsi_ring_cursor: 0,
                sampler_linear,
                uber_vs,
                uber_fs,
                uber_dsl,
                uber_pipe_layout,
                desc_pool,
                uber_desc_set,
                offscreen_rp,
                offscreen_pipe,
                offscreen_target: None,
                cache_rp,
                cache_pipe,
                tex_cache,
                cache_fb,
                cache_valid_key: 0,
                pending_cache: None,
                globals_buf,
                instance_buf,
                staging_buf,
                tex_r8,
                tex_rgba,
                tex_wallpaper,
                tex_glass,
                tex_photo,
                photo_uploaded_id: 0,
                atlas_epoch_seen: 0,
                wallpaper_phys_size: [2944.0, 1840.0],
                kawase_dsl,
                kawase_pipe_layout,
                kawase_rp,
                kawase_down_pipe,
                kawase_up_pipe,
                kawase_ubos,
                kawase_sets,
                wsi_surfaces: HashMap::new(),
                wsi_pipes: HashMap::new(),
            };

            renderer.init_atlases_and_wallpaper()?;
            renderer.update_uber_descriptors();
            Ok(renderer)
        }
    }

    unsafe fn find_memory_type(
        mem_props: &vk::PhysicalDeviceMemoryProperties,
        type_bits: u32,
        req: vk::MemoryPropertyFlags,
    ) -> Option<u32> {
        for i in 0..mem_props.memory_type_count {
            if (type_bits & (1 << i)) != 0
                && mem_props.memory_types[i as usize]
                    .property_flags
                    .contains(req)
            {
                return Some(i);
            }
        }
        None
    }

    unsafe fn alloc_host_buffer(
        device: &ash::Device,
        mem_props: &vk::PhysicalDeviceMemoryProperties,
        size: usize,
        usage: vk::BufferUsageFlags,
    ) -> Result<GpuBuffer, String> {
        let buffer = device
            .create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size as u64)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )
            .map_err(|e| e.to_string())?;
        let req = device.get_buffer_memory_requirements(buffer);
        // Prefer HOST_CACHED for fast readback when available, else HOST_COHERENT
        let mem_idx = Self::find_memory_type(
            mem_props,
            req.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT
                | vk::MemoryPropertyFlags::HOST_CACHED,
        )
        .or_else(|| {
            Self::find_memory_type(
                mem_props,
                req.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
        })
        .ok_or("no host-visible memory type")?;

        let memory = device
            .allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(mem_idx),
                None,
            )
            .map_err(|e| e.to_string())?;
        device
            .bind_buffer_memory(buffer, memory, 0)
            .map_err(|e| e.to_string())?;
        let mapped = device
            .map_memory(memory, 0, size as u64, vk::MemoryMapFlags::empty())
            .map_err(|e| e.to_string())? as *mut u8;
        Ok(GpuBuffer {
            buffer,
            memory,
            mapped,
            size,
        })
    }

    unsafe fn alloc_image(
        device: &ash::Device,
        mem_props: &vk::PhysicalDeviceMemoryProperties,
        width: u32,
        height: u32,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
    ) -> Result<GpuImage, String> {
        let image = device
            .create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(format)
                    .extent(vk::Extent3D {
                        width: width.max(1),
                        height: height.max(1),
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(usage)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )
            .map_err(|e| e.to_string())?;
        let req = device.get_image_memory_requirements(image);
        let mem_idx = Self::find_memory_type(
            mem_props,
            req.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )
        .or_else(|| Self::find_memory_type(mem_props, req.memory_type_bits, vk::MemoryPropertyFlags::empty()))
        .ok_or("no image memory type")?;
        let memory = device
            .allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(mem_idx),
                None,
            )
            .map_err(|e| e.to_string())?;
        device
            .bind_image_memory(image, memory, 0)
            .map_err(|e| e.to_string())?;
        let view = device
            .create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    }),
                None,
            )
            .map_err(|e| e.to_string())?;
        Ok(GpuImage {
            image,
            memory,
            view,
            width: width.max(1),
            height: height.max(1),
            format,
            layout: vk::ImageLayout::UNDEFINED,
        })
    }

    unsafe fn destroy_image(&self, img: &GpuImage) {
        self.device.destroy_image_view(img.view, None);
        self.device.destroy_image(img.image, None);
        self.device.free_memory(img.memory, None);
    }

    unsafe fn create_render_pass(
        device: &ash::Device,
        format: vk::Format,
        final_layout: vk::ImageLayout,
        clear: bool,
    ) -> Result<vk::RenderPass, String> {
        let att = [vk::AttachmentDescription::default()
            .format(format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(if clear {
                vk::AttachmentLoadOp::CLEAR
            } else {
                vk::AttachmentLoadOp::DONT_CARE
            })
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(final_layout)];
        let color_ref = [vk::AttachmentReference {
            attachment: 0,
            layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        }];
        let subpass = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_ref)];
        let dep = [vk::SubpassDependency::default()
            .src_subpass(vk::SUBPASS_EXTERNAL)
            .dst_subpass(0)
            .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT | vk::PipelineStageFlags::FRAGMENT_SHADER)
            .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT | vk::PipelineStageFlags::FRAGMENT_SHADER)
            .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE | vk::AccessFlags::SHADER_READ)];
        device
            .create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(&att)
                    .subpasses(&subpass)
                    .dependencies(&dep),
                None,
            )
            .map_err(|e| e.to_string())
    }

    unsafe fn create_uber_pipeline(
        device: &ash::Device,
        layout: vk::PipelineLayout,
        render_pass: vk::RenderPass,
        vs: vk::ShaderModule,
        fs: vk::ShaderModule,
        blend: bool,
    ) -> Result<vk::Pipeline, String> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vs)
                .name(c"vs_main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fs)
                .name(c"fs_main"),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let ia = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let vp_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let rast = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let ms = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let blend_att = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(blend)
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_att);
        let dyn_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dyn_info = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dyn_states);

        let ci = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&ia)
            .viewport_state(&vp_state)
            .rasterization_state(&rast)
            .multisample_state(&ms)
            .color_blend_state(&cb)
            .dynamic_state(&dyn_info)
            .layout(layout)
            .render_pass(render_pass)
            .subpass(0)];
        device
            .create_graphics_pipelines(vk::PipelineCache::null(), &ci, None)
            .map_err(|(_, e)| format!("create_uber_pipeline: {e}"))
            .map(|v| v[0])
    }

    unsafe fn create_kawase_pipeline_named(
        device: &ash::Device,
        layout: vk::PipelineLayout,
        render_pass: vk::RenderPass,
        vs: vk::ShaderModule,
        fs: vk::ShaderModule,
        is_up: bool,
    ) -> Result<vk::Pipeline, String> {
        // Wait: kawase_down_fs has entry `fs_down`, kawase_up_fs has entry `fs_up`!
        let fs_name = if is_up { c"fs_up" } else { c"fs_down" };
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vs)
                .name(c"vs_fullscreen"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fs)
                .name(fs_name),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let ia = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let vp_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let rast = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let ms = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let blend_att = [vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(false)
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let cb = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_att);
        let dyn_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dyn_info = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dyn_states);
        let ci = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&ia)
            .viewport_state(&vp_state)
            .rasterization_state(&rast)
            .multisample_state(&ms)
            .color_blend_state(&cb)
            .dynamic_state(&dyn_info)
            .layout(layout)
            .render_pass(render_pass)
            .subpass(0)];
        device
            .create_graphics_pipelines(vk::PipelineCache::null(), &ci, None)
            .map_err(|(_, e)| format!("create_kawase_pipeline: {e}"))
            .map(|v| v[0])
    }

    unsafe fn transition_image(
        device: &ash::Device,
        cmd: vk::CommandBuffer,
        img: &mut GpuImage,
        new_layout: vk::ImageLayout,
    ) {
        if img.layout == new_layout {
            return;
        }
        let barrier = [vk::ImageMemoryBarrier::default()
            .old_layout(img.layout)
            .new_layout(new_layout)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(img.image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .src_access_mask(vk::AccessFlags::MEMORY_WRITE | vk::AccessFlags::MEMORY_READ)
            .dst_access_mask(vk::AccessFlags::MEMORY_WRITE | vk::AccessFlags::MEMORY_READ)];
        device.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &barrier,
        );
        img.layout = new_layout;
    }

    unsafe fn init_atlases_and_wallpaper(&mut self) -> Result<(), String> {
        // Synthesize or load wallpaper (1472 x 920 RGBA8)
        let wp_w = self.tex_wallpaper.width as usize;
        let wp_h = self.tex_wallpaper.height as usize;
        let wp_bytes = wp_w * wp_h * 4;
        let dst = std::slice::from_raw_parts_mut(self.staging_buf.mapped, wp_bytes);
        populate_active_wallpaper_pixels(dst, wp_w as u32, wp_h as u32, false);
        *wallpaper_monet_store().lock().unwrap() =
            crate::color::WallpaperMonet::extract(dst, wp_w, wp_h, false);

        // Zero-clear 64x64 dummy photo slot right after wallpaper in staging
        let photo_off = wp_bytes;
        let photo_bytes = 64 * 64 * 4;
        std::ptr::write_bytes(self.staging_buf.mapped.add(photo_off), 0, photo_bytes);

        self.begin_cmd()?;
        Self::transition_image(
            &self.device,
            self.cmd_buf,
            &mut self.tex_r8,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        );
        self.device.cmd_clear_color_image(
            self.cmd_buf,
            self.tex_r8.image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &vk::ClearColorValue { float32: [0.0; 4] },
            &[vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            }],
        );
        Self::transition_image(
            &self.device,
            self.cmd_buf,
            &mut self.tex_r8,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        );

        Self::transition_image(
            &self.device,
            self.cmd_buf,
            &mut self.tex_rgba,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        );
        self.device.cmd_clear_color_image(
            self.cmd_buf,
            self.tex_rgba.image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &vk::ClearColorValue { float32: [0.0; 4] },
            &[vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            }],
        );
        Self::transition_image(
            &self.device,
            self.cmd_buf,
            &mut self.tex_rgba,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        );

        Self::transition_image(
            &self.device,
            self.cmd_buf,
            &mut self.tex_wallpaper,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        );
        self.device.cmd_copy_buffer_to_image(
            self.cmd_buf,
            self.staging_buf.buffer,
            self.tex_wallpaper.image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[vk::BufferImageCopy {
                buffer_offset: 0,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                image_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
                image_extent: vk::Extent3D {
                    width: wp_w as u32,
                    height: wp_h as u32,
                    depth: 1,
                },
            }],
        );
        Self::transition_image(
            &self.device,
            self.cmd_buf,
            &mut self.tex_wallpaper,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        );

        Self::transition_image(
            &self.device,
            self.cmd_buf,
            &mut self.tex_photo,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        );
        self.device.cmd_copy_buffer_to_image(
            self.cmd_buf,
            self.staging_buf.buffer,
            self.tex_photo.image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[vk::BufferImageCopy {
                buffer_offset: photo_off as u64,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                image_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
                image_extent: vk::Extent3D {
                    width: 64,
                    height: 64,
                    depth: 1,
                },
            }],
        );
        Self::transition_image(
            &self.device,
            self.cmd_buf,
            &mut self.tex_photo,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        );
        Self::transition_image(
            &self.device,
            self.cmd_buf,
            &mut self.tex_cache,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        );
        self.device.cmd_clear_color_image(
            self.cmd_buf,
            self.tex_cache.image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &vk::ClearColorValue { float32: [0.0; 4] },
            &[vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            }],
        );
        Self::transition_image(
            &self.device,
            self.cmd_buf,
            &mut self.tex_cache,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        );
        self.submit_and_wait()?;

        // Run 4-pass Dual-Kawase downsample + 4-pass Dual-Kawase upsample pyramid on GPU!
        self.run_kawase_wallpaper_blur()?;
        Ok(())
    }

    /// Hot-reloads `tex_wallpaper`, updates `WallpaperMonet`, and regenerates the 8-pass Dual-Kawase
    /// blurred `tex_glass` backdrop so Lockscreen and Standby Companion reflect live wallpaper changes!
    pub fn reload_wallpaper(&mut self) -> Result<(), String> {
        let _ = self.flush_pending_work();
        unsafe {
            let wp_w = self.tex_wallpaper.width as usize;
            let wp_h = self.tex_wallpaper.height as usize;
            let wp_bytes = wp_w * wp_h * 4;
            let dst = std::slice::from_raw_parts_mut(self.staging_buf.mapped, wp_bytes);
            populate_active_wallpaper_pixels(dst, wp_w as u32, wp_h as u32, false);
            *wallpaper_monet_store().lock().unwrap() =
                crate::color::WallpaperMonet::extract(dst, wp_w, wp_h, false);

            self.begin_cmd()?;
            Self::transition_image(
                &self.device,
                self.cmd_buf,
                &mut self.tex_wallpaper,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            );
            self.device.cmd_copy_buffer_to_image(
                self.cmd_buf,
                self.staging_buf.buffer,
                self.tex_wallpaper.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::BufferImageCopy {
                    buffer_offset: 0,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    image_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
                    image_extent: vk::Extent3D {
                        width: wp_w as u32,
                        height: wp_h as u32,
                        depth: 1,
                    },
                }],
            );
            Self::transition_image(
                &self.device,
                self.cmd_buf,
                &mut self.tex_wallpaper,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            );
            self.submit_and_wait()?;
            self.run_kawase_wallpaper_blur()?;
        }
        Ok(())
    }

    unsafe fn run_kawase_wallpaper_blur(&mut self) -> Result<(), String> {
        let mem_props = self.instance.get_physical_device_memory_properties(self.phys_dev);
        let dims = [(736u32, 460u32), (368, 230), (184, 115), (92, 58)];
        let mut pyr: Vec<(GpuImage, vk::Framebuffer)> = Vec::with_capacity(4);
        for &(w, h) in &dims {
            let img = Self::alloc_image(
                &self.device,
                &mem_props,
                w,
                h,
                vk::Format::R8G8B8A8_UNORM,
                vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
            )?;
            let fb = self
                .device
                .create_framebuffer(
                    &vk::FramebufferCreateInfo::default()
                        .render_pass(self.kawase_rp)
                        .attachments(std::slice::from_ref(&img.view))
                        .width(w)
                        .height(h)
                        .layers(1),
                    None,
                )
                .map_err(|e| e.to_string())?;
            pyr.push((img, fb));
        }
        let glass_fb = self
            .device
            .create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(self.kawase_rp)
                    .attachments(std::slice::from_ref(&self.tex_glass.view))
                    .width(self.tex_glass.width)
                    .height(self.tex_glass.height)
                    .layers(1),
                None,
            )
            .map_err(|e| e.to_string())?;

        // 8 passes total:
        // 0: wallpaper(1472x920) -> pyr[0](736x460) down
        // 1: pyr[0] -> pyr[1](368x230) down
        // 2: pyr[1] -> pyr[2](184x115) down
        // 3: pyr[2] -> pyr[3](92x58) down
        // 4: pyr[3] -> pyr[2](184x115) up
        // 5: pyr[2] -> pyr[1](368x230) up
        // 6: pyr[1] -> pyr[0](736x460) up
        // 7: pyr[0] -> tex_glass(736x460) up + 1.62x saturation boost!
        let passes: [(vk::ImageView, u32, u32, vk::Framebuffer, u32, u32, bool, f32, f32); 8] = [
            (self.tex_wallpaper.view, 1472, 920, pyr[0].1, 736, 460, false, 1.5, 1.0),
            (pyr[0].0.view, 736, 460, pyr[1].1, 368, 230, false, 2.0, 1.0),
            (pyr[1].0.view, 368, 230, pyr[2].1, 184, 115, false, 2.5, 1.0),
            (pyr[2].0.view, 184, 115, pyr[3].1, 92, 58, false, 3.0, 1.0),
            (pyr[3].0.view, 92, 58, pyr[2].1, 184, 115, true, 2.5, 1.0),
            (pyr[2].0.view, 184, 115, pyr[1].1, 368, 230, true, 2.0, 1.0),
            (pyr[1].0.view, 368, 230, pyr[0].1, 736, 460, true, 1.5, 1.0),
            (pyr[0].0.view, 736, 460, glass_fb, 736, 460, true, 1.25, 1.62),
        ];

        for (i, &(src_view, sw, sh, _, _, _, _, offset, sat_boost)) in passes.iter().enumerate() {
            let u = BlurUniform {
                texel: [1.0 / (sw as f32), 1.0 / (sh as f32)],
                offset,
                sat_boost,
            };
            std::ptr::copy_nonoverlapping(
                &u as *const BlurUniform as *const u8,
                self.kawase_ubos[i].mapped,
                std::mem::size_of::<BlurUniform>(),
            );
            let b_info = [vk::DescriptorBufferInfo {
                buffer: self.kawase_ubos[i].buffer,
                offset: 0,
                range: std::mem::size_of::<BlurUniform>() as u64,
            }];
            let s_info = [vk::DescriptorImageInfo {
                sampler: self.sampler_linear,
                image_view: vk::ImageView::null(),
                image_layout: vk::ImageLayout::UNDEFINED,
            }];
            let i_info = [vk::DescriptorImageInfo {
                sampler: vk::Sampler::null(),
                image_view: src_view,
                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            }];
            let writes = [
                vk::WriteDescriptorSet::default()
                    .dst_set(self.kawase_sets[i])
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                    .buffer_info(&b_info),
                vk::WriteDescriptorSet::default()
                    .dst_set(self.kawase_sets[i])
                    .dst_binding(1)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .image_info(&s_info),
                vk::WriteDescriptorSet::default()
                    .dst_set(self.kawase_sets[i])
                    .dst_binding(2)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(&i_info),
            ];
            self.device.update_descriptor_sets(&writes, &[]);
        }

        self.begin_cmd()?;
        for (i, &(_, _, _, dst_fb, dw, dh, is_up, _, _)) in passes.iter().enumerate() {
            let rp_bi = vk::RenderPassBeginInfo::default()
                .render_pass(self.kawase_rp)
                .framebuffer(dst_fb)
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: vk::Extent2D { width: dw, height: dh },
                });
            self.device
                .cmd_begin_render_pass(self.cmd_buf, &rp_bi, vk::SubpassContents::INLINE);
            self.device.cmd_bind_pipeline(
                self.cmd_buf,
                vk::PipelineBindPoint::GRAPHICS,
                if is_up {
                    self.kawase_up_pipe
                } else {
                    self.kawase_down_pipe
                },
            );
            self.device.cmd_set_viewport(
                self.cmd_buf,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: dw as f32,
                    height: dh as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            self.device.cmd_set_scissor(
                self.cmd_buf,
                0,
                &[vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: vk::Extent2D { width: dw, height: dh },
                }],
            );
            self.device.cmd_bind_descriptor_sets(
                self.cmd_buf,
                vk::PipelineBindPoint::GRAPHICS,
                self.kawase_pipe_layout,
                0,
                std::slice::from_ref(&self.kawase_sets[i]),
                &[],
            );
            self.device.cmd_draw(self.cmd_buf, 3, 1, 0, 0);
            self.device.cmd_end_render_pass(self.cmd_buf);
        }
        self.tex_glass.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        self.submit_and_wait()?;

        self.device.destroy_framebuffer(glass_fb, None);
        for (img, fb) in pyr {
            self.device.destroy_framebuffer(fb, None);
            self.destroy_image(&img);
        }
        Ok(())
    }

    unsafe fn wait_all_wsi_fences(&self) {
        if !self.wsi_ring.is_empty() {
            let fences: Vec<vk::Fence> = self.wsi_ring.iter().map(|s| s.fence).collect();
            let _ = self.device.wait_for_fences(&fences, true, u64::MAX);
        }
    }

    unsafe fn write_uber_set_descriptors(
        &self,
        dst_set: vk::DescriptorSet,
        globals_buf: vk::Buffer,
        inst_buf: vk::Buffer,
        inst_cap: usize,
        photo_view: vk::ImageView,
    ) {
        let g_info = [vk::DescriptorBufferInfo {
            buffer: globals_buf,
            offset: 0,
            range: std::mem::size_of::<GlobalsUniform>() as u64,
        }];
        let s_info = [vk::DescriptorBufferInfo {
            buffer: inst_buf,
            offset: 0,
            range: (inst_cap * std::mem::size_of::<SdfInstance>()) as u64,
        }];
        let samp_info = [vk::DescriptorImageInfo {
            sampler: self.sampler_linear,
            image_view: vk::ImageView::null(),
            image_layout: vk::ImageLayout::UNDEFINED,
        }];
        let r8_info = [vk::DescriptorImageInfo {
            sampler: vk::Sampler::null(),
            image_view: self.tex_r8.view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let rgba_info = [vk::DescriptorImageInfo {
            sampler: vk::Sampler::null(),
            image_view: self.tex_rgba.view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let glass_info = [vk::DescriptorImageInfo {
            sampler: vk::Sampler::null(),
            image_view: self.tex_glass.view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let photo_info = [vk::DescriptorImageInfo {
            sampler: vk::Sampler::null(),
            image_view: photo_view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        let cache_info = [vk::DescriptorImageInfo {
            sampler: vk::Sampler::null(),
            image_view: self.tex_cache.view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];

        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(dst_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(&g_info),
            vk::WriteDescriptorSet::default()
                .dst_set(dst_set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&s_info),
            vk::WriteDescriptorSet::default()
                .dst_set(dst_set)
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .image_info(&samp_info),
            vk::WriteDescriptorSet::default()
                .dst_set(dst_set)
                .dst_binding(3)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&r8_info),
            vk::WriteDescriptorSet::default()
                .dst_set(dst_set)
                .dst_binding(4)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&rgba_info),
            vk::WriteDescriptorSet::default()
                .dst_set(dst_set)
                .dst_binding(5)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&glass_info),
            vk::WriteDescriptorSet::default()
                .dst_set(dst_set)
                .dst_binding(6)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&photo_info),
            vk::WriteDescriptorSet::default()
                .dst_set(dst_set)
                .dst_binding(7)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&cache_info),
        ];
        self.device.update_descriptor_sets(&writes, &[]);
    }

    unsafe fn update_uber_descriptors(&self) {
        self.wait_all_wsi_fences();
        let photo_view = if self.photo_uploaded_id == 1 {
            self.tex_wallpaper.view
        } else {
            self.tex_photo.view
        };
        self.write_uber_set_descriptors(
            self.uber_desc_set,
            self.globals_buf.buffer,
            self.instance_buf.buffer,
            MAX_INSTANCES,
            photo_view,
        );
        for slot in &self.wsi_ring {
            self.write_uber_set_descriptors(
                slot.desc_set,
                slot.globals_buf.buffer,
                slot.instance_buf.buffer,
                RING_MAX_INSTANCES,
                photo_view,
            );
        }
    }

    unsafe fn update_photo_slot_view(&self, view: vk::ImageView) {
        self.wait_all_wsi_fences();
        let wp_info = [vk::DescriptorImageInfo {
            sampler: vk::Sampler::null(),
            image_view: view,
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        }];
        self.device.update_descriptor_sets(
            &[vk::WriteDescriptorSet::default()
                .dst_set(self.uber_desc_set)
                .dst_binding(6)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&wp_info)],
            &[],
        );
        for slot in &self.wsi_ring {
            self.device.update_descriptor_sets(
                &[vk::WriteDescriptorSet::default()
                    .dst_set(slot.desc_set)
                    .dst_binding(6)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(&wp_info)],
                &[],
            );
        }
    }

    unsafe fn begin_cmd(&self) -> Result<(), String> {
        self.device
            .wait_for_fences(&[self.fence], true, u64::MAX)
            .map_err(|e| e.to_string())?;
        self.device
            .reset_fences(&[self.fence])
            .map_err(|e| e.to_string())?;
        self.device
            .reset_command_buffer(self.cmd_buf, vk::CommandBufferResetFlags::empty())
            .map_err(|e| e.to_string())?;
        self.device
            .begin_command_buffer(
                self.cmd_buf,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .map_err(|e| e.to_string())
    }

    unsafe fn submit_and_wait(&self) -> Result<(), String> {
        self.device
            .end_command_buffer(self.cmd_buf)
            .map_err(|e| e.to_string())?;
        let submits = [vk::SubmitInfo::default().command_buffers(std::slice::from_ref(&self.cmd_buf))];
        self.device
            .queue_submit(self.queue, &submits, self.fence)
            .map_err(|e| e.to_string())?;
        self.device
            .wait_for_fences(&[self.fence], true, u64::MAX)
            .map_err(|e| e.to_string())
    }

    /// Flush queued glyph/icon/photo uploads from `AtlasBank::global()` into GPU VRAM.
    unsafe fn flush_atlas_uploads(&mut self) -> Result<(), String> {
        let (epoch, uploads, photo_req) = {
            let mut bank = AtlasBank::global().lock().unwrap();
            bank.take_pending()
        };

        // 1. Optional full-res photo/wallpaper slot update (id == 1 means draw raw wallpaper!)
        if let Some((pid, pw, ph, pixels)) = photo_req {
            if pid != self.photo_uploaded_id && pw > 0 && ph > 0 && pixels.len() == (pw * ph * 4) as usize {
                let mem_props = self.instance.get_physical_device_memory_properties(self.phys_dev);
                if self.tex_photo.width != pw || self.tex_photo.height != ph {
                    let new_img = Self::alloc_image(
                        &self.device,
                        &mem_props,
                        pw,
                        ph,
                        vk::Format::R8G8B8A8_UNORM,
                        vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST,
                    )?;
                    self.destroy_image(&self.tex_photo);
                    self.tex_photo = new_img;
                    self.update_uber_descriptors();
                }
                if pixels.len() <= self.staging_buf.size {
                    std::ptr::copy_nonoverlapping(pixels.as_ptr(), self.staging_buf.mapped, pixels.len());
                    self.begin_cmd()?;
                    Self::transition_image(
                        &self.device,
                        self.cmd_buf,
                        &mut self.tex_photo,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    );
                    self.device.cmd_copy_buffer_to_image(
                        self.cmd_buf,
                        self.staging_buf.buffer,
                        self.tex_photo.image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &[vk::BufferImageCopy {
                            buffer_offset: 0,
                            buffer_row_length: 0,
                            buffer_image_height: 0,
                            image_subresource: vk::ImageSubresourceLayers {
                                aspect_mask: vk::ImageAspectFlags::COLOR,
                                mip_level: 0,
                                base_array_layer: 0,
                                layer_count: 1,
                            },
                            image_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
                            image_extent: vk::Extent3D {
                                width: pw,
                                height: ph,
                                depth: 1,
                            },
                        }],
                    );
                    Self::transition_image(
                        &self.device,
                        self.cmd_buf,
                        &mut self.tex_photo,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    );
                    self.submit_and_wait()?;
                    self.update_photo_slot_view(self.tex_photo.view);
                    self.photo_uploaded_id = pid;
                }
            }
        } else if self.photo_uploaded_id == 0 {
            // Bind `tex_wallpaper` to `tex_photo` descriptor initially so `draw_wallpaper_background` works!
            self.update_photo_slot_view(self.tex_wallpaper.view);
            self.photo_uploaded_id = 1;
        }

        if uploads.is_empty() && epoch == self.atlas_epoch_seen {
            return Ok(());
        }
        self.atlas_epoch_seen = epoch;
        if uploads.is_empty() {
            return Ok(());
        }

        let mut cursor = 0usize;
        let mut r8_copies: Vec<vk::BufferImageCopy> = Vec::new();
        let mut rgba_copies: Vec<vk::BufferImageCopy> = Vec::new();

        for up in &uploads {
            let aligned = (cursor + 3) & !3;
            if aligned + up.data.len() > self.staging_buf.size {
                break;
            }
            std::ptr::copy_nonoverlapping(
                up.data.as_ptr(),
                self.staging_buf.mapped.add(aligned),
                up.data.len(),
            );
            let bic = vk::BufferImageCopy {
                buffer_offset: aligned as u64,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                image_offset: vk::Offset3D {
                    x: up.x as i32,
                    y: up.y as i32,
                    z: 0,
                },
                image_extent: vk::Extent3D {
                    width: up.w,
                    height: up.h,
                    depth: 1,
                },
            };
            match up.target {
                AtlasTarget::R8 => r8_copies.push(bic),
                AtlasTarget::Rgba => rgba_copies.push(bic),
            }
            cursor = aligned + up.data.len();
        }

        self.begin_cmd()?;
        if !r8_copies.is_empty() {
            Self::transition_image(
                &self.device,
                self.cmd_buf,
                &mut self.tex_r8,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            );
            self.device.cmd_copy_buffer_to_image(
                self.cmd_buf,
                self.staging_buf.buffer,
                self.tex_r8.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &r8_copies,
            );
            Self::transition_image(
                &self.device,
                self.cmd_buf,
                &mut self.tex_r8,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            );
        }
        if !rgba_copies.is_empty() {
            Self::transition_image(
                &self.device,
                self.cmd_buf,
                &mut self.tex_rgba,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            );
            self.device.cmd_copy_buffer_to_image(
                self.cmd_buf,
                self.staging_buf.buffer,
                self.tex_rgba.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &rgba_copies,
            );
            Self::transition_image(
                &self.device,
                self.cmd_buf,
                &mut self.tex_rgba,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            );
        }
        self.submit_and_wait()
    }

    pub fn bind_wallpaper_to_photo_slot(&mut self) {
        if self.photo_uploaded_id != 1 {
            unsafe {
                self.update_photo_slot_view(self.tex_wallpaper.view);
                self.photo_uploaded_id = 1;
            }
        }
    }

    unsafe fn prepare_globals_and_instances(
        &mut self,
        phys_w: u32,
        phys_h: u32,
        surf_origin: [f32; 2],
        wallpaper_phys: [f32; 2],
        instances: &[SdfInstance],
    ) -> usize {
        // A presented frame is not waited for when it is submitted (see `present_wayland_wsi`), so the
        // GPU may still be reading these buffers. Wait here, right before overwriting them.
        let t_wait = crate::perf::start();
        let _ = self.device.wait_for_fences(&[self.fence], true, u64::MAX);
        crate::perf::end("  vk.prev_frame_wait", t_wait);
        let globals = GlobalsUniform {
            target_size: [phys_w as f32, phys_h as f32],
            wallpaper_size: wallpaper_phys,
            surf_origin,
            extra: [0.0; 2],
        };
        std::ptr::copy_nonoverlapping(
            &globals as *const GlobalsUniform as *const u8,
            self.globals_buf.mapped,
            std::mem::size_of::<GlobalsUniform>(),
        );
        let count = instances.len().min(MAX_INSTANCES);
        if count > 0 {
            std::ptr::copy_nonoverlapping(
                instances.as_ptr() as *const u8,
                self.instance_buf.mapped,
                count * std::mem::size_of::<SdfInstance>(),
            );
        }
        count
    }

    unsafe fn ensure_offscreen(&mut self, w: u32, h: u32) -> Result<(), String> {
        let w = w.max(1);
        let h = h.max(1);
        if let Some((img, _)) = &self.offscreen_target {
            if img.width == w && img.height == h {
                return Ok(());
            }
        }
        if let Some((old_img, old_fb)) = self.offscreen_target.take() {
            self.device.destroy_framebuffer(old_fb, None);
            self.destroy_image(&old_img);
        }
        let mem_props = self.instance.get_physical_device_memory_properties(self.phys_dev);
        let img = Self::alloc_image(
            &self.device,
            &mem_props,
            w,
            h,
            vk::Format::R8G8B8A8_UNORM,
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;
        let fb = self
            .device
            .create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(self.offscreen_rp)
                    .attachments(std::slice::from_ref(&img.view))
                    .width(w)
                    .height(h)
                    .layers(1),
                None,
            )
            .map_err(|e| e.to_string())?;
        self.offscreen_target = Some((img, fb));
        Ok(())
    }

    pub fn has_valid_cache(&self, key: u64, phys_w: u32, phys_h: u32) -> bool {
        key != 0
            && self.cache_valid_key == key
            && self.tex_cache.width == phys_w.max(1)
            && self.tex_cache.height == phys_h.max(1)
    }

    pub fn invalidate_cache(&mut self) {
        self.cache_valid_key = 0;
        self.pending_cache = None;
    }

    unsafe fn ensure_cache_target(&mut self, w: u32, h: u32) -> Result<(), String> {
        let w = w.max(1);
        let h = h.max(1);
        if self.tex_cache.width == w && self.tex_cache.height == h {
            return Ok(());
        }
        let _ = self.device.wait_for_fences(&[self.fence], true, u64::MAX);
        self.wait_all_wsi_fences();
        let mem_props = self.instance.get_physical_device_memory_properties(self.phys_dev);
        let new_img = Self::alloc_image(
            &self.device,
            &mem_props,
            w,
            h,
            vk::Format::R8G8B8A8_UNORM,
            vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::COLOR_ATTACHMENT,
        )?;
        let new_fb = self
            .device
            .create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(self.cache_rp)
                    .attachments(std::slice::from_ref(&new_img.view))
                    .width(w)
                    .height(h)
                    .layers(1),
                None,
            )
            .map_err(|e| e.to_string())?;
        self.device.destroy_framebuffer(self.cache_fb, None);
        self.destroy_image(&self.tex_cache);
        self.tex_cache = new_img;
        self.cache_fb = new_fb;
        self.update_uber_descriptors();
        Ok(())
    }

    unsafe fn execute_cache_pass_sync(&mut self, pc: &PendingCachePass) -> Result<(), String> {
        let w = pc.phys_w.max(1);
        let h = pc.phys_h.max(1);
        self.ensure_cache_target(w, h)?;
        let count = self.prepare_globals_and_instances(
            w,
            h,
            pc.surf_origin,
            pc.wallpaper_phys,
            &pc.instances,
        );
        let ca = (pc.clear.a as f32) / 255.0;
        let clear_val = [vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [
                    (pc.clear.r as f32 / 255.0) * ca,
                    (pc.clear.g as f32 / 255.0) * ca,
                    (pc.clear.b as f32 / 255.0) * ca,
                    ca,
                ],
            },
        }];

        self.begin_cmd()?;
        let rp_bi = vk::RenderPassBeginInfo::default()
            .render_pass(self.cache_rp)
            .framebuffer(self.cache_fb)
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D { width: w, height: h },
            })
            .clear_values(&clear_val);
        self.device
            .cmd_begin_render_pass(self.cmd_buf, &rp_bi, vk::SubpassContents::INLINE);
        self.device.cmd_bind_pipeline(
            self.cmd_buf,
            vk::PipelineBindPoint::GRAPHICS,
            self.cache_pipe,
        );
        self.device.cmd_set_viewport(
            self.cmd_buf,
            0,
            &[vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: w as f32,
                height: h as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            }],
        );
        self.device.cmd_set_scissor(
            self.cmd_buf,
            0,
            &[vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D { width: w, height: h },
            }],
        );
        if count > 0 {
            self.device.cmd_bind_descriptor_sets(
                self.cmd_buf,
                vk::PipelineBindPoint::GRAPHICS,
                self.uber_pipe_layout,
                0,
                std::slice::from_ref(&self.uber_desc_set),
                &[],
            );
            self.device.cmd_draw(self.cmd_buf, 6, count as u32, 0, 0);
        }
        self.device.cmd_end_render_pass(self.cmd_buf);
        self.tex_cache.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        self.submit_and_wait()
    }

    /// Flush all queued glyph/icon uploads and any deferred `tex_cache` pass synchronously (for startup warm-up).
    pub fn flush_pending_work(&mut self) -> Result<(), String> {
        unsafe {
            self.flush_atlas_uploads()?;
            if let Some(pc) = self.pending_cache.take() {
                self.execute_cache_pass_sync(&pc)?;
            }
            Ok(())
        }
    }

    /// Pre-allocate a Wayland WSI swapchain before the surface is mapped so the first interactive
    /// gesture incurs `0.0 ms` swapchain/DMA-BUF creation cost.
    pub fn prewarm_wsi_swapchain(
        &mut self,
        surf_id: SurfaceId,
        wl_display_ptr: *mut c_void,
        wl_surface_ptr: *mut c_void,
        phys_w: u32,
        phys_h: u32,
    ) -> Result<(), String> {
        if !self.is_hardware_gpu || wl_display_ptr.is_null() || wl_surface_ptr.is_null() {
            return Ok(());
        }
        unsafe {
            self.ensure_wsi_swapchain(surf_id, wl_display_ptr, wl_surface_ptr, phys_w, phys_h)
        }
    }

    /// Queue `instances` to bake into `tex_cache` (`R8G8B8A8_UNORM`, premultiplied RGBA, `SHADER_READ_ONLY_OPTIMAL`)
    /// in the same GPU command buffer as the next `present_wayland_wsi` (`0.0 ms` CPU fence wait!).
    #[allow(clippy::too_many_arguments)]
    pub fn render_to_cache(
        &mut self,
        key: u64,
        phys_w: u32,
        phys_h: u32,
        clear: Rgba,
        surf_origin: [f32; 2],
        wallpaper_phys: [f32; 2],
        instances: &[SdfInstance],
    ) -> Result<(), String> {
        let w = phys_w.max(1);
        let h = phys_h.max(1);
        unsafe {
            self.flush_atlas_uploads()?;
            self.ensure_cache_target(w, h)?;
            let pass = PendingCachePass {
                phys_w: w,
                phys_h: h,
                clear,
                surf_origin,
                wallpaper_phys,
                instances: instances.to_vec(),
            };
            if self.is_hardware_gpu {
                self.pending_cache = Some(pass);
                self.cache_valid_key = key;
                Ok(())
            } else {
                self.execute_cache_pass_sync(&pass)?;
                self.cache_valid_key = key;
                Ok(())
            }
        }
    }

    /// Render `instances` offscreen on Vulkan GPU and copy premultiplied RGBA -> Wayland `Argb8888` (`BGRA`) or PNG!
    pub fn render_offscreen_rgba(
        &mut self,
        phys_w: u32,
        phys_h: u32,
        clear: Rgba,
        surf_origin: [f32; 2],
        wallpaper_phys: [f32; 2],
        instances: &[SdfInstance],
    ) -> Result<&[u8], String> {
        unsafe {
            let needed = (phys_w as usize) * (phys_h as usize) * 4;
            if needed > self.staging_buf.size {
                return Err("frame exceeds staging buffer capacity".into());
            }
            self.flush_atlas_uploads()?;
            if let Some(pc) = self.pending_cache.take() {
                self.execute_cache_pass_sync(&pc)?;
            }
            self.ensure_offscreen(phys_w, phys_h)?;
            let count = self.prepare_globals_and_instances(
                phys_w,
                phys_h,
                surf_origin,
                wallpaper_phys,
                instances,
            );
            let (target_img, target_fb) = self.offscreen_target.as_ref().unwrap();
            let target_image_handle = target_img.image;
            let fb_handle = *target_fb;

            let ca = (clear.a as f32) / 255.0;
            let clear_val = [vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [
                        (clear.r as f32 / 255.0) * ca,
                        (clear.g as f32 / 255.0) * ca,
                        (clear.b as f32 / 255.0) * ca,
                        ca,
                    ],
                },
            }];

            self.begin_cmd()?;
            let rp_bi = vk::RenderPassBeginInfo::default()
                .render_pass(self.offscreen_rp)
                .framebuffer(fb_handle)
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: vk::Extent2D {
                        width: phys_w,
                        height: phys_h,
                    },
                })
                .clear_values(&clear_val);
            self.device
                .cmd_begin_render_pass(self.cmd_buf, &rp_bi, vk::SubpassContents::INLINE);
            self.device.cmd_bind_pipeline(
                self.cmd_buf,
                vk::PipelineBindPoint::GRAPHICS,
                self.offscreen_pipe,
            );
            self.device.cmd_set_viewport(
                self.cmd_buf,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: phys_w as f32,
                    height: phys_h as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            self.device.cmd_set_scissor(
                self.cmd_buf,
                0,
                &[vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: vk::Extent2D {
                        width: phys_w,
                        height: phys_h,
                    },
                }],
            );
            if count > 0 {
                self.device.cmd_bind_descriptor_sets(
                    self.cmd_buf,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.uber_pipe_layout,
                    0,
                    std::slice::from_ref(&self.uber_desc_set),
                    &[],
                );
                self.device.cmd_draw(self.cmd_buf, 6, count as u32, 0, 0);
            }
            self.device.cmd_end_render_pass(self.cmd_buf);

            self.device.cmd_copy_image_to_buffer(
                self.cmd_buf,
                target_image_handle,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.staging_buf.buffer,
                &[vk::BufferImageCopy {
                    buffer_offset: 0,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    image_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
                    image_extent: vk::Extent3D {
                        width: phys_w,
                        height: phys_h,
                        depth: 1,
                    },
                }],
            );
            self.submit_and_wait()?;
            Ok(std::slice::from_raw_parts(self.staging_buf.mapped, needed))
        }
    }

    /// Zero-copy DMA-BUF Wayland WSI presentation via `VK_KHR_wayland_surface` + `VK_KHR_swapchain`!
    /// Uses a 4-slot frame ring (`wsi_ring`) so multiple layer surfaces presenting in the same vblank
    /// never wait on each other's GPU fences, and fuses any queued `tex_cache` bake into a single GPU submit!
    #[allow(clippy::too_many_arguments)]
    pub fn present_wayland_wsi(
        &mut self,
        surf_id: SurfaceId,
        wl_display_ptr: *mut c_void,
        wl_surface_ptr: *mut c_void,
        phys_w: u32,
        phys_h: u32,
        clear: Rgba,
        surf_origin: [f32; 2],
        wallpaper_phys: [f32; 2],
        instances: &[SdfInstance],
    ) -> Result<(), String> {
        if wl_display_ptr.is_null() || wl_surface_ptr.is_null() {
            return Err("null wayland native pointers".into());
        }
        if !self.is_hardware_gpu {
            // Prefer offscreen + SHM when running on software lavapipe so WSI never stalls
            return Err("software ICD -> prefer offscreen".into());
        }
        unsafe {
            self.flush_atlas_uploads()?;
            self.ensure_wsi_swapchain(surf_id, wl_display_ptr, wl_surface_ptr, phys_w, phys_h)?;

            // Check if a queued `tex_cache` bake can be fused into this exact ring slot submission
            let mut inline_cache: Option<PendingCachePass> = None;
            if let Some(pc) = self.pending_cache.take() {
                if pc.phys_w == phys_w.max(1)
                    && pc.phys_h == phys_h.max(1)
                    && pc.surf_origin == surf_origin
                    && pc.wallpaper_phys == wallpaper_phys
                    && pc.instances.len() + instances.len() <= RING_MAX_INSTANCES
                {
                    inline_cache = Some(pc);
                } else {
                    self.execute_cache_pass_sync(&pc)?;
                }
            }

            let ring_idx = self.wsi_ring_cursor % WSI_RING;
            self.wsi_ring_cursor = self.wsi_ring_cursor.wrapping_add(1);
            let slot_cmd = self.wsi_ring[ring_idx].cmd_buf;
            let slot_fence = self.wsi_ring[ring_idx].fence;
            let slot_desc = self.wsi_ring[ring_idx].desc_set;
            let slot_globals_ptr = self.wsi_ring[ring_idx].globals_buf.mapped;
            let slot_inst_ptr = self.wsi_ring[ring_idx].instance_buf.mapped;

            let t_wait = crate::perf::start();
            let _ = self.device.wait_for_fences(&[slot_fence], true, u64::MAX);
            crate::perf::end("  vk.prev_frame_wait", t_wait);

            let globals = GlobalsUniform {
                target_size: [phys_w as f32, phys_h as f32],
                wallpaper_size: wallpaper_phys,
                surf_origin,
                extra: [0.0; 2],
            };
            std::ptr::copy_nonoverlapping(
                &globals as *const GlobalsUniform as *const u8,
                slot_globals_ptr,
                std::mem::size_of::<GlobalsUniform>(),
            );

            let cache_count = if let Some(ref pc) = inline_cache {
                let c0 = pc.instances.len().min(RING_MAX_INSTANCES);
                if c0 > 0 {
                    std::ptr::copy_nonoverlapping(
                        pc.instances.as_ptr() as *const u8,
                        slot_inst_ptr,
                        c0 * std::mem::size_of::<SdfInstance>(),
                    );
                }
                c0
            } else {
                0
            };

            let max_main = RING_MAX_INSTANCES.saturating_sub(cache_count);
            let count = instances.len().min(max_main);
            if count > 0 {
                std::ptr::copy_nonoverlapping(
                    instances.as_ptr() as *const u8,
                    slot_inst_ptr.add(cache_count * std::mem::size_of::<SdfInstance>()),
                    count * std::mem::size_of::<SdfInstance>(),
                );
            }

            let sc_ext = self.swapchain_ext.as_ref().ok_or("no swapchain ext")?;
            let ws = self.wsi_surfaces.get(&surf_id).ok_or("missing wsi surface")?;

            let t_acq = crate::perf::start();
            let (img_idx, _) = match sc_ext.acquire_next_image(
                ws.swapchain,
                100_000_000, // 100ms
                ws.image_avail,
                vk::Fence::null(),
            ) {
                Ok(pair) => pair,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                    if let Some(pc) = inline_cache {
                        let _ = self.execute_cache_pass_sync(&pc);
                    }
                    self.destroy_wsi_surface(surf_id);
                    return Err("swapchain out of date".into());
                }
                Err(e) => {
                    if let Some(pc) = inline_cache {
                        let _ = self.execute_cache_pass_sync(&pc);
                    }
                    return Err(format!("acquire_next_image: {e}"));
                }
            };
            crate::perf::end("  vk.acquire", t_acq);

            let ca = (clear.a as f32) / 255.0;
            let clear_val = [vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [
                        (clear.r as f32 / 255.0) * ca,
                        (clear.g as f32 / 255.0) * ca,
                        (clear.b as f32 / 255.0) * ca,
                        ca,
                    ],
                },
            }];

            let fb = ws.framebuffers[img_idx as usize];
            let rp = ws.render_pass;
            let pipe = ws.pipeline;
            let extent = ws.extent;
            let wait_sem = ws.image_avail;
            let sig_sem = ws.render_done;
            let swapchain = ws.swapchain;

            self.device
                .reset_fences(&[slot_fence])
                .map_err(|e| e.to_string())?;
            self.device
                .reset_command_buffer(slot_cmd, vk::CommandBufferResetFlags::empty())
                .map_err(|e| e.to_string())?;
            self.device
                .begin_command_buffer(
                    slot_cmd,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(|e| e.to_string())?;

            // Optional fused `tex_cache` pass right before swapchain pass (`0` extra submit, `0` CPU wait!)
            if let Some(ref pc) = inline_cache {
                let cw = pc.phys_w.max(1);
                let ch = pc.phys_h.max(1);
                let cca = (pc.clear.a as f32) / 255.0;
                let c_clear = [vk::ClearValue {
                    color: vk::ClearColorValue {
                        float32: [
                            (pc.clear.r as f32 / 255.0) * cca,
                            (pc.clear.g as f32 / 255.0) * cca,
                            (pc.clear.b as f32 / 255.0) * cca,
                            cca,
                        ],
                    },
                }];
                let cache_bi = vk::RenderPassBeginInfo::default()
                    .render_pass(self.cache_rp)
                    .framebuffer(self.cache_fb)
                    .render_area(vk::Rect2D {
                        offset: vk::Offset2D { x: 0, y: 0 },
                        extent: vk::Extent2D { width: cw, height: ch },
                    })
                    .clear_values(&c_clear);
                self.device
                    .cmd_begin_render_pass(slot_cmd, &cache_bi, vk::SubpassContents::INLINE);
                self.device.cmd_bind_pipeline(
                    slot_cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.cache_pipe,
                );
                self.device.cmd_set_viewport(
                    slot_cmd,
                    0,
                    &[vk::Viewport {
                        x: 0.0,
                        y: 0.0,
                        width: cw as f32,
                        height: ch as f32,
                        min_depth: 0.0,
                        max_depth: 1.0,
                    }],
                );
                self.device.cmd_set_scissor(
                    slot_cmd,
                    0,
                    &[vk::Rect2D {
                        offset: vk::Offset2D { x: 0, y: 0 },
                        extent: vk::Extent2D { width: cw, height: ch },
                    }],
                );
                if cache_count > 0 {
                    self.device.cmd_bind_descriptor_sets(
                        slot_cmd,
                        vk::PipelineBindPoint::GRAPHICS,
                        self.uber_pipe_layout,
                        0,
                        std::slice::from_ref(&slot_desc),
                        &[],
                    );
                    self.device.cmd_draw(slot_cmd, 6, cache_count as u32, 0, 0);
                }
                self.device.cmd_end_render_pass(slot_cmd);
                self.tex_cache.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;

                let cache_barrier = [vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(self.tex_cache.image)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ)];
                self.device.cmd_pipeline_barrier(
                    slot_cmd,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &cache_barrier,
                );
            }

            let rp_bi = vk::RenderPassBeginInfo::default()
                .render_pass(rp)
                .framebuffer(fb)
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent,
                })
                .clear_values(&clear_val);
            self.device
                .cmd_begin_render_pass(slot_cmd, &rp_bi, vk::SubpassContents::INLINE);
            self.device
                .cmd_bind_pipeline(slot_cmd, vk::PipelineBindPoint::GRAPHICS, pipe);
            self.device.cmd_set_viewport(
                slot_cmd,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: extent.width as f32,
                    height: extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            self.device.cmd_set_scissor(
                slot_cmd,
                0,
                &[vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent,
                }],
            );
            if count > 0 {
                self.device.cmd_bind_descriptor_sets(
                    slot_cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.uber_pipe_layout,
                    0,
                    std::slice::from_ref(&slot_desc),
                    &[],
                );
                self.device
                    .cmd_draw(slot_cmd, 6, count as u32, 0, cache_count as u32);
            }
            self.device.cmd_end_render_pass(slot_cmd);
            self.device
                .end_command_buffer(slot_cmd)
                .map_err(|e| e.to_string())?;

            let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
            let submits = [vk::SubmitInfo::default()
                .wait_semaphores(std::slice::from_ref(&wait_sem))
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(std::slice::from_ref(&slot_cmd))
                .signal_semaphores(std::slice::from_ref(&sig_sem))];
            self.device
                .queue_submit(self.queue, &submits, slot_fence)
                .map_err(|e| e.to_string())?;

            let indices = [img_idx];
            let swaps = [swapchain];
            let pres_info = vk::PresentInfoKHR::default()
                .wait_semaphores(std::slice::from_ref(&sig_sem))
                .swapchains(&swaps)
                .image_indices(&indices);
            let t_pres = crate::perf::start();
            let _ = sc_ext.queue_present(self.queue, &pres_info);
            crate::perf::end("  vk.queue_present", t_pres);
            if crate::perf::enabled() {
                crate::perf::gauge(&format!("instances[{surf_id}]"), count as u64);
                let target_px = (phys_w as f64) * (phys_h as f64);
                let shaded: f64 = instances[..count]
                    .iter()
                    .filter(|i| (i.params[0] + 0.5) as i32 != 6)
                    .map(|i| (i.rect[2].max(0.0) as f64) * (i.rect[3].max(0.0) as f64))
                    .sum();
                crate::perf::gauge(&format!("overdraw%[{surf_id}]"), (shaded * 100.0 / target_px.max(1.0)) as u64);
                if crate::perf::sync_gpu() {
                    let t_gpu = crate::perf::start();
                    let _ = self.device.wait_for_fences(&[slot_fence], true, u64::MAX);
                    crate::perf::end(&format!("gpu_time[{surf_id}]"), t_gpu);
                }
            }
            Ok(())
        }
    }

    unsafe fn ensure_wsi_swapchain(
        &mut self,
        surf_id: SurfaceId,
        wl_display_ptr: *mut c_void,
        wl_surface_ptr: *mut c_void,
        phys_w: u32,
        phys_h: u32,
    ) -> Result<(), String> {
        let target_ext = vk::Extent2D {
            width: phys_w.max(1),
            height: phys_h.max(1),
        };
        if let Some(existing) = self.wsi_surfaces.get(&surf_id) {
            if existing.extent == target_ext {
                return Ok(());
            }
        }

        let (Some(surf_ext), Some(wl_ext), Some(sc_ext)) = (
            self.surface_ext.as_ref(),
            self.wl_surface_ext.as_ref(),
            self.swapchain_ext.as_ref(),
        ) else {
            return Err("Vulkan WSI extensions not loaded".into());
        };

        let prev_surface_khr = self.wsi_surfaces.remove(&surf_id).map(|old| {
            let _ = self.device.device_wait_idle();
            for fb in old.framebuffers {
                self.device.destroy_framebuffer(fb, None);
            }
            for v in old.views {
                self.device.destroy_image_view(v, None);
            }
            self.device.destroy_semaphore(old.image_avail, None);
            self.device.destroy_semaphore(old.render_done, None);
            sc_ext.destroy_swapchain(old.swapchain, None);
            old.surface_khr
        });

        let surface_khr = match prev_surface_khr {
            Some(s) => s,
            None => {
                if !wl_ext.get_physical_device_wayland_presentation_support(
                    self.phys_dev,
                    self.queue_family,
                    &mut *(wl_display_ptr as *mut vk::wl_display),
                ) {
                    return Err("physical device lacks Wayland presentation support".into());
                }
                let ci = vk::WaylandSurfaceCreateInfoKHR::default()
                    .display(wl_display_ptr as *mut vk::wl_display)
                    .surface(wl_surface_ptr as *mut vk::wl_surface);
                wl_ext
                    .create_wayland_surface(&ci, None)
                    .map_err(|e| format!("create_wayland_surface: {e}"))?
            }
        };

        let caps = surf_ext
            .get_physical_device_surface_capabilities(self.phys_dev, surface_khr)
            .map_err(|e| e.to_string())?;
        let formats = surf_ext
            .get_physical_device_surface_formats(self.phys_dev, surface_khr)
            .map_err(|e| e.to_string())?;
        let chosen_fmt = formats
            .iter()
            .find(|f| {
                f.format == vk::Format::B8G8R8A8_UNORM
                    && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
            })
            .or_else(|| formats.iter().find(|f| f.format == vk::Format::R8G8B8A8_UNORM))
            .copied()
            .ok_or("no UNORM swapchain format")?;

        let comp_alpha = if caps
            .supported_composite_alpha
            .contains(vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED)
        {
            vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED
        } else if caps
            .supported_composite_alpha
            .contains(vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED)
        {
            vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED
        } else {
            vk::CompositeAlphaFlagsKHR::OPAQUE
        };

        // MAILBOX never blocks in `queue_present`; frames are paced by `wl_surface.frame` callbacks in
        // `WlConn` instead. FIFO (always available) blocks until the compositor took the previous
        // frame, and for a surface that is not being shown that is a ~1 s stall per present.
        let present_mode = surf_ext
            .get_physical_device_surface_present_modes(self.phys_dev, surface_khr)
            .ok()
            .filter(|modes| modes.contains(&vk::PresentModeKHR::MAILBOX))
            .map(|_| vk::PresentModeKHR::MAILBOX)
            .unwrap_or(vk::PresentModeKHR::FIFO);
        // The driver's minimum is enough: at most one frame is in flight per surface, and every extra
        // image of a full-screen surface is ~22 MB.
        let img_count = if caps.max_image_count > 0 {
            caps.min_image_count.min(caps.max_image_count)
        } else {
            caps.min_image_count
        };

        let sc_ci = vk::SwapchainCreateInfoKHR::default()
            .surface(surface_khr)
            .min_image_count(img_count)
            .image_format(chosen_fmt.format)
            .image_color_space(chosen_fmt.color_space)
            .image_extent(target_ext)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(comp_alpha)
            .present_mode(present_mode)
            .clipped(true);

        let swapchain = sc_ext
            .create_swapchain(&sc_ci, None)
            .map_err(|e| format!("create_swapchain: {e}"))?;
        let images = sc_ext
            .get_swapchain_images(swapchain)
            .map_err(|e| e.to_string())?;

        let (render_pass, pipeline) = match self.wsi_pipes.get(&chosen_fmt.format) {
            Some(&pair) => pair,
            None => {
                let render_pass = Self::create_render_pass(
                    &self.device,
                    chosen_fmt.format,
                    vk::ImageLayout::PRESENT_SRC_KHR,
                    true,
                )?;
                let pipeline = Self::create_uber_pipeline(
                    &self.device,
                    self.uber_pipe_layout,
                    render_pass,
                    self.uber_vs,
                    self.uber_fs,
                    true,
                )?;
                self.wsi_pipes.insert(chosen_fmt.format, (render_pass, pipeline));
                (render_pass, pipeline)
            }
        };

        let mut views = Vec::with_capacity(images.len());
        let mut framebuffers = Vec::with_capacity(images.len());
        for &img in &images {
            let v = self
                .device
                .create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(img)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(chosen_fmt.format)
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: 0,
                            layer_count: 1,
                        }),
                    None,
                )
                .map_err(|e| e.to_string())?;
            let fb = self
                .device
                .create_framebuffer(
                    &vk::FramebufferCreateInfo::default()
                        .render_pass(render_pass)
                        .attachments(std::slice::from_ref(&v))
                        .width(target_ext.width)
                        .height(target_ext.height)
                        .layers(1),
                    None,
                )
                .map_err(|e| e.to_string())?;
            views.push(v);
            framebuffers.push(fb);
        }

        let image_avail = self
            .device
            .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
            .map_err(|e| e.to_string())?;
        let render_done = self
            .device
            .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
            .map_err(|e| e.to_string())?;

        self.wsi_surfaces.insert(
            surf_id,
            SwapchainSurface {
                surface_khr,
                swapchain,
                format: chosen_fmt.format,
                extent: target_ext,
                images,
                views,
                framebuffers,
                render_pass,
                pipeline,
                image_avail,
                render_done,
            },
        );
        Ok(())
    }

    pub fn destroy_wsi_surface(&mut self, surf_id: SurfaceId) {
        if let Some(old) = self.wsi_surfaces.remove(&surf_id) {
            unsafe {
                let _ = self.device.device_wait_idle();
                for fb in old.framebuffers {
                    self.device.destroy_framebuffer(fb, None);
                }
                for v in old.views {
                    self.device.destroy_image_view(v, None);
                }
                self.device.destroy_semaphore(old.image_avail, None);
                self.device.destroy_semaphore(old.render_done, None);
                if let Some(sc_ext) = &self.swapchain_ext {
                    sc_ext.destroy_swapchain(old.swapchain, None);
                }
                if let Some(surf_ext) = &self.surface_ext {
                    surf_ext.destroy_surface(old.surface_khr, None);
                }
            }
        }
    }
}

fn wallpaper_monet_store() -> &'static Mutex<crate::color::WallpaperMonet> {
    static MONET: OnceLock<Mutex<crate::color::WallpaperMonet>> = OnceLock::new();
    MONET.get_or_init(|| Mutex::new(crate::color::WallpaperMonet::default()))
}

fn wallpaper_spec_store() -> &'static Mutex<String> {
    static SPEC: OnceLock<Mutex<String>> = OnceLock::new();
    SPEC.get_or_init(|| Mutex::new("stock".to_string()))
}

pub fn set_active_wallpaper_spec(spec: &str) {
    let normalized = if spec.trim().is_empty() {
        "stock"
    } else {
        spec.trim()
    };
    *wallpaper_spec_store().lock().unwrap() = normalized.to_string();
}

pub fn active_wallpaper_spec() -> String {
    wallpaper_spec_store().lock().unwrap().clone()
}

pub fn active_wallpaper_monet() -> crate::color::WallpaperMonet {
    *wallpaper_monet_store().lock().unwrap()
}

/// Renders the active user/ZUI wallpaper at full native `(w, h)` resolution with aspect-preserving
/// `object-fit: cover` bilinear sampling directly into a Wayland `Argb8888` (`BGRA`) slice,
/// updating and returning the extracted `WallpaperMonet` palette.
pub fn render_wallpaper_bgra_and_monet(
    dst_bgra: &mut [u8],
    w: u32,
    h: u32,
) -> crate::color::WallpaperMonet {
    let need = (w as usize) * (h as usize) * 4;
    if dst_bgra.len() < need || w == 0 || h == 0 {
        return active_wallpaper_monet();
    }
    populate_active_wallpaper_pixels(&mut dst_bgra[..need], w, h, true);
    let monet = crate::color::WallpaperMonet::extract(&dst_bgra[..need], w as usize, h as usize, true);
    *wallpaper_monet_store().lock().unwrap() = monet;
    monet
}

fn populate_active_wallpaper_pixels(dst: &mut [u8], w: u32, h: u32, bgra_out: bool) {
    let spec = active_wallpaper_spec();
    match spec.as_str() {
        "plume" => {
            synthesize_plume_wallpaper(dst, w as usize, h as usize, bgra_out);
            return;
        }
        "aurora" => {
            synthesize_radial_preset_wallpaper(
                dst,
                w as usize,
                h as usize,
                bgra_out,
                (10.0, 18.0, 34.0),
                (15.0, 28.0, 48.0),
                &[
                    (0.32, 0.38, 0.56, 16.0, 185.0, 148.0, 0.72),
                    (0.74, 0.34, 0.50, 34.0, 152.0, 232.0, 0.68),
                    (0.52, 0.68, 0.48, 52.0, 211.0, 173.0, 0.52),
                    (0.18, 0.78, 0.46, 29.0, 78.0, 168.0, 0.64),
                ],
            );
            return;
        }
        "twilight" => {
            synthesize_radial_preset_wallpaper(
                dst,
                w as usize,
                h as usize,
                bgra_out,
                (18.0, 14.0, 36.0),
                (28.0, 20.0, 52.0),
                &[
                    (0.68, 0.36, 0.54, 225.0, 72.0, 118.0, 0.72),
                    (0.28, 0.42, 0.52, 112.0, 78.0, 235.0, 0.70),
                    (0.54, 0.72, 0.46, 245.0, 152.0, 92.0, 0.54),
                    (0.20, 0.80, 0.44, 62.0, 52.0, 138.0, 0.66),
                ],
            );
            return;
        }
        "mist" => {
            synthesize_radial_preset_wallpaper(
                dst,
                w as usize,
                h as usize,
                bgra_out,
                (224.0, 232.0, 244.0),
                (198.0, 210.0, 228.0),
                &[
                    (0.30, 0.34, 0.54, 154.0, 192.0, 246.0, 0.58),
                    (0.76, 0.38, 0.48, 204.0, 196.0, 244.0, 0.54),
                    (0.52, 0.74, 0.52, 244.0, 248.0, 255.0, 0.68),
                    (0.18, 0.80, 0.44, 168.0, 188.0, 218.0, 0.52),
                ],
            );
            return;
        }
        _ => {}
    }

    if let Some(custom_path) = spec
        .strip_prefix("custom:")
        .or_else(|| spec.starts_with('/').then_some(spec.as_str()))
    {
        if load_specific_wallpaper_file(Path::new(custom_path.trim()), dst, w, h, bgra_out) {
            return;
        }
    }

    if !load_user_wallpaper_into(dst, w, h, bgra_out) {
        synthesize_plume_wallpaper(dst, w as usize, h as usize, bgra_out);
    }
}

fn load_specific_wallpaper_file(
    path: &Path,
    dst: &mut [u8],
    w: u32,
    h: u32,
    bgra_out: bool,
) -> bool {
    if !path.is_file() {
        return false;
    }
    let Some(bmp) = BitmapIcon::from_photo_file(path, 3200) else {
        return false;
    };
    let sw = bmp.pixmap.width() as usize;
    let sh = bmp.pixmap.height() as usize;
    if sw == 0 || sh == 0 {
        return false;
    }
    let src = bmp.pixmap.data();
    let dw = w as usize;
    let dh = h as usize;
    let scale = ((dw as f32) / (sw as f32)).max((dh as f32) / (sh as f32));
    let vis_sw = ((dw as f32) / scale).min(sw as f32);
    let vis_sh = ((dh as f32) / scale).min(sh as f32);
    let off_x = ((sw as f32) - vis_sw) * 0.5;
    let off_y = ((sh as f32) - vis_sh) * 0.5;

    for dy in 0..dh {
        let fy = (off_y + ((dy as f32 + 0.5) / (dh as f32)) * vis_sh - 0.5)
            .clamp(0.0, (sh.saturating_sub(1)) as f32);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(sh - 1);
        let wy = fy - (y0 as f32);
        let wy0 = 1.0 - wy;

        for dx in 0..dw {
            let fx = (off_x + ((dx as f32 + 0.5) / (dw as f32)) * vis_sw - 0.5)
                .clamp(0.0, (sw.saturating_sub(1)) as f32);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(sw - 1);
            let wx = fx - (x0 as f32);
            let wx0 = 1.0 - wx;

            let i00 = (y0 * sw + x0) * 4;
            let i10 = (y0 * sw + x1) * 4;
            let i01 = (y1 * sw + x0) * 4;
            let i11 = (y1 * sw + x1) * 4;

            let mut ch = [0u8; 4];
            for c in 0..3 {
                let v0 = (src[i00 + c] as f32) * wx0 + (src[i10 + c] as f32) * wx;
                let v1 = (src[i01 + c] as f32) * wx0 + (src[i11 + c] as f32) * wx;
                ch[c] = (v0 * wy0 + v1 * wy).round().clamp(0.0, 255.0) as u8;
            }
            ch[3] = 255;

            let di = (dy * dw + dx) * 4;
            if bgra_out {
                dst[di] = ch[2];
                dst[di + 1] = ch[1];
                dst[di + 2] = ch[0];
                dst[di + 3] = 255;
            } else {
                dst[di..di + 4].copy_from_slice(&ch);
            }
        }
    }
    true
}

fn load_user_wallpaper_into(dst: &mut [u8], w: u32, h: u32, bgra_out: bool) -> bool {
    let mut candidates = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        for ext in ["jpg", "jpeg", "png"] {
            candidates.push(format!("{home}/Pictures/Wallpaper.{ext}"));
            candidates.push(format!("{home}/.config/tb520fu/wallpaper.{ext}"));
        }
    }
    candidates.push("/usr/share/tb520fu/wallpaper.jpg".into());
    candidates.push("/usr/share/tb520fu/wallpaper.png".into());
    candidates.push("assets/wallpaper.jpg".into());
    candidates.push(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/wallpaper.jpg").into());

    for p in candidates {
        if load_specific_wallpaper_file(Path::new(&p), dst, w, h, bgra_out) {
            return true;
        }
    }
    false
}

fn synthesize_radial_preset_wallpaper(
    dst: &mut [u8],
    w: usize,
    h: usize,
    bgra_out: bool,
    base_tl: (f32, f32, f32),
    base_br: (f32, f32, f32),
    blobs: &[(f32, f32, f32, f32, f32, f32, f32)],
) {
    let wf = w as f32;
    let hf = h as f32;
    let max_dim = wf.max(hf);
    let diag_denom = (wf * wf + hf * hf).max(1.0);
    for y in 0..h {
        let py = y as f32;
        for x in 0..w {
            let px = x as f32;
            let gt = ((px * wf + py * hf) / diag_denom).clamp(0.0, 1.0);
            let mut r = base_tl.0 + (base_br.0 - base_tl.0) * gt;
            let mut g = base_tl.1 + (base_br.1 - base_tl.1) * gt;
            let mut b = base_tl.2 + (base_br.2 - base_tl.2) * gt;

            for &(bx, by, br, cr, cg, cb, ca) in blobs {
                let rad = max_dim * br;
                let dx = px - wf * bx;
                let dy = py - hf * by;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist < rad {
                    let t = (dist / rad).clamp(0.0, 1.0);
                    let falloff = (1.0 - t) * (1.0 - t * t * 0.22);
                    let a = (ca * falloff).clamp(0.0, 1.0);
                    r = r * (1.0 - a) + cr * a;
                    g = g * (1.0 - a) + cg * a;
                    b = b * (1.0 - a) + cb * a;
                }
            }

            let idx = (y * w + x) * 4;
            let rr = r.clamp(0.0, 255.0) as u8;
            let gg = g.clamp(0.0, 255.0) as u8;
            let bb = b.clamp(0.0, 255.0) as u8;
            if bgra_out {
                dst[idx] = bb;
                dst[idx + 1] = gg;
                dst[idx + 2] = rr;
            } else {
                dst[idx] = rr;
                dst[idx + 1] = gg;
                dst[idx + 2] = bb;
            }
            dst[idx + 3] = 255;
        }
    }
}

/// ZUI 流羽暖金 (`WALLS[0]` `'plume'` from Prototype v9) procedural default wallpaper:
/// brushed silver-slate base (`#a8b0bd` -> `#8b95a5`), 5 warm amber/cream/slate radial plumes,
/// and a `-0.36 rad` tilted warm golden feather ribbon across the center.
fn synthesize_plume_wallpaper(dst: &mut [u8], w: usize, h: usize, bgra_out: bool) {
    let wf = w as f32;
    let hf = h as f32;
    let max_dim = wf.max(hf);
    let diag_denom = (wf * wf + hf * hf).max(1.0);

    // `(bx, by, br, r, g, b, alpha)` from `WALLS[0].blobs` in `desktop-proto/index.html`
    const BLOBS: [(f32, f32, f32, f32, f32, f32, f32); 5] = [
        (0.52, 0.48, 0.46, 245.0, 165.0, 96.0, 0.92),
        (0.28, 0.35, 0.44, 255.0, 238.0, 218.0, 0.90),
        (0.78, 0.32, 0.42, 248.0, 196.0, 132.0, 0.85),
        (0.20, 0.78, 0.48, 92.0, 118.0, 148.0, 0.88),
        (0.68, 0.82, 0.42, 116.0, 136.0, 162.0, 0.82),
    ];

    let cos_a = 0.36_f32.cos();
    let sin_a = 0.36_f32.sin();
    let rx = (wf * 0.42).max(1.0);
    let ry = (hf * 0.13).max(1.0);
    let pcx = wf * 0.48;
    let pcy = hf * 0.52;
    let grad_span = (wf * 0.70).max(1.0);
    let grad_x0 = -wf * 0.35;

    for y in 0..h {
        let py = y as f32;
        for x in 0..w {
            let px = x as f32;

            // 1. Diagonal linear gradient `(0,0) -> (w,h)`: `#a8b0bd` -> `#8b95a5`
            let gt = ((px * wf + py * hf) / diag_denom).clamp(0.0, 1.0);
            let mut r = 168.0 + (139.0 - 168.0) * gt;
            let mut g = 176.0 + (149.0 - 176.0) * gt;
            let mut b = 189.0 + (165.0 - 189.0) * gt;

            // 2. Source-over radial gradient blobs
            for &(bx, by, br, cr, cg, cb, ca) in &BLOBS {
                let rad = max_dim * br;
                let dx = px - wf * bx;
                let dy = py - hf * by;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist < rad {
                    let t = (dist / rad).clamp(0.0, 1.0);
                    // Linear radial stop with soft outer taper so 8-bit gradients never band
                    let falloff = (1.0 - t) * (1.0 - t * t * 0.18);
                    let a = (ca * falloff).clamp(0.0, 1.0);
                    r = r * (1.0 - a) + cr * a;
                    g = g * (1.0 - a) + cg * a;
                    b = b * (1.0 - a) + cb * a;
                }
            }

            // 3. Tilted warm golden plume ellipse at `(0.48w, 0.52h)`, rotated `-0.36 rad`
            let dx = px - pcx;
            let dy = py - pcy;
            let lx = dx * cos_a - dy * sin_a;
            let ly = dx * sin_a + dy * cos_a;
            let er = ((lx / rx) * (lx / rx) + (ly / ry) * (ly / ry)).sqrt();
            if er < 1.04 {
                let edge_t = ((1.04 - er) / 0.14).clamp(0.0, 1.0);
                let edge_cov = edge_t * edge_t * (3.0 - 2.0 * edge_t);
                let pt = ((lx - grad_x0) / grad_span).clamp(0.0, 1.0);
                let (pr, pg, pb, pa) = if pt <= 0.5 {
                    let u = pt * 2.0;
                    (
                        255.0,
                        248.0 + (222.0 - 248.0) * u,
                        236.0 + (175.0 - 236.0) * u,
                        0.12 + (0.62 - 0.12) * u,
                    )
                } else {
                    let u = (pt - 0.5) * 2.0;
                    (
                        255.0 + (242.0 - 255.0) * u,
                        222.0 + (148.0 - 222.0) * u,
                        175.0 + (78.0 - 175.0) * u,
                        0.62 + (0.14 - 0.62) * u,
                    )
                };
                let a = (pa * edge_cov).clamp(0.0, 1.0);
                r = r * (1.0 - a) + pr * a;
                g = g * (1.0 - a) + pg * a;
                b = b * (1.0 - a) + pb * a;
            }

            let idx = (y * w + x) * 4;
            let rr = r.clamp(0.0, 255.0) as u8;
            let gg = g.clamp(0.0, 255.0) as u8;
            let bb = b.clamp(0.0, 255.0) as u8;
            if bgra_out {
                dst[idx] = bb;
                dst[idx + 1] = gg;
                dst[idx + 2] = rr;
            } else {
                dst[idx] = rr;
                dst[idx + 1] = gg;
                dst[idx + 2] = bb;
            }
            dst[idx + 3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::canvas::{Canvas, RectF};
    use crate::color::{Palette, Rgba, Skin};
    use crate::svg_icons::IconSet;
    use crate::text::{Align, FontWeight, TextEngine};

    #[test]
    fn vulkan_uber_sdf_and_kawase_glass_render() {
        let mut canvas = Canvas::new(320, 180, 1.0);
        let pal = Palette::for_skin(Skin::GlassDark);
        canvas.draw_wallpaper_background(RectF::new(0.0, 0.0, 320.0, 180.0), 1.0);
        canvas.fill_glass_panel(RectF::new(20.0, 20.0, 280.0, 140.0), 24.0, &pal, 1.0);
        let icons = IconSet::new();
        icons.draw(
            &mut canvas,
            "wifi",
            RectF::new(40.0, 40.0, 24.0, 24.0),
            Rgba::hex(0x93c5fd),
            2.0,
        );
        let mut txt = TextEngine::new();
        txt.draw_in_rect(
            &mut canvas,
            "TB520FU Vulkan 1.4",
            RectF::new(72.0, 38.0, 210.0, 28.0),
            18.0,
            FontWeight::Bold,
            pal.ink,
            Align::Left,
        );
        let mut bgra = vec![0u8; 320 * 180 * 4];
        assert!(canvas.render_to_wayland_argb8888(&mut bgra));
        let non_zero = bgra
            .chunks_exact(4)
            .filter(|px| px[3] > 0 && (px[0] > 0 || px[1] > 0 || px[2] > 0))
            .count();
        assert!(
            non_zero > 320 * 170,
            "expected full rendered frame, got {non_zero}"
        );
    }
}
