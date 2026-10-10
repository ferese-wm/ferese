"""Render the matching Ferese wallpapers directly at UHD resolution.

Run with Blender 4.5 or later:
    blender --background --python assets/wallpapers/render.py

Pass -- --preview to render 960 x 540 previews instead.
"""

import math
import sys
from pathlib import Path

import bpy
from mathutils import Vector


ROOT = Path(__file__).resolve().parent
PREVIEW = "--preview" in sys.argv
DESTINATION = Path("/tmp/ferese-wallpaper-render") if PREVIEW else ROOT


def material(name, color, metallic=0.0, roughness=0.35):
    result = bpy.data.materials.new(name)
    result.use_nodes = True
    shader = result.node_tree.nodes.get("Principled BSDF")
    shader.inputs["Base Color"].default_value = (*color, 1.0)
    shader.inputs["Metallic"].default_value = metallic
    shader.inputs["Roughness"].default_value = roughness
    shader.inputs["Coat Weight"].default_value = 0.3
    shader.inputs["Coat Roughness"].default_value = 0.2
    return result


def ribbon(name, phase, offset, surface):
    vertices, faces = [], []
    length_steps, width_steps = 360, 64
    for i in range(length_steps):
        t = i / length_steps
        angle = 2.0 * math.pi * t
        x = 2.0 + 5.4 * math.cos(angle)
        y = 3.5 * math.sin(angle)
        z = 0.8 * math.sin(2.0 * angle) + offset
        tangent = Vector((-5.4 * math.sin(angle), 3.5 * math.cos(angle), 0.0))
        normal = Vector((-tangent.y, tangent.x, 0.0)).normalized()
        width = 1.75 + 0.25 * math.cos(angle + phase)
        twist = angle + phase
        for j in range(width_steps + 1):
            u = 2.0 * j / width_steps - 1.0
            distance = width * u * math.cos(twist)
            curl = 0.25 * width * u * u
            vertices.append(
                (
                    x + normal.x * distance,
                    y + normal.y * distance,
                    z + width * u * math.sin(twist) + curl,
                )
            )
    for i in range(length_steps):
        for j in range(width_steps):
            a = i * (width_steps + 1) + j
            b = ((i + 1) % length_steps) * (width_steps + 1) + j
            faces.append((a, a + 1, b + 1, b))
    mesh = bpy.data.meshes.new(name)
    mesh.from_pydata(vertices, [], faces)
    mesh.update()
    obj = bpy.data.objects.new(name, mesh)
    bpy.context.collection.objects.link(obj)
    obj.data.materials.append(surface)
    for polygon in mesh.polygons:
        polygon.use_smooth = True
    solid = obj.modifiers.new("Satin edge", "SOLIDIFY")
    solid.thickness = 0.028
    bevel = obj.modifiers.new("Soft edge", "BEVEL")
    bevel.width = 0.025
    bevel.segments = 3
    return obj


def area(name, position, color, power, size, target=(1.0, 0.0, 0.0)):
    light = bpy.data.lights.new(name, "AREA")
    light.energy = power
    light.color = color
    light.shape = "RECTANGLE"
    light.size = size
    light.size_y = size * 2.0
    obj = bpy.data.objects.new(name, light)
    bpy.context.collection.objects.link(obj)
    obj.location = position
    obj.rotation_euler = (Vector(target) - obj.location).to_track_quat("-Z", "Y").to_euler()


def render(appearance):
    bpy.ops.wm.read_factory_settings(use_empty=True)
    scene = bpy.context.scene
    scene.render.engine = "CYCLES"
    scene.cycles.samples = 24 if PREVIEW else 64
    scene.cycles.use_denoising = True
    scene.cycles.adaptive_threshold = 0.03
    scene.cycles.max_bounces = 6
    scene.render.resolution_x = 960 if PREVIEW else 3840
    scene.render.resolution_y = 540 if PREVIEW else 2160
    scene.render.resolution_percentage = 100
    scene.render.image_settings.file_format = "PNG"
    scene.render.image_settings.color_mode = "RGB"
    scene.render.image_settings.color_depth = "8"
    scene.render.film_transparent = False
    scene.view_settings.view_transform = "AgX"
    scene.view_settings.look = "AgX - Medium High Contrast"

    dark = appearance == "dark"
    world = bpy.data.worlds.new("Studio")
    world.use_nodes = True
    world.node_tree.nodes["Background"].inputs["Color"].default_value = (
        (0.11, 0.16, 0.27, 1.0) if dark else (0.75, 0.82, 0.95, 1.0)
    )
    world.node_tree.nodes["Background"].inputs["Strength"].default_value = 0.35 if dark else 0.55
    scene.world = world

    pearl = material("Pearl satin", (0.12, 0.43, 0.66) if dark else (0.56, 0.75, 0.95), 0.45, 0.28)
    blue = material("Blue satin", (0.014, 0.085, 0.30) if dark else (0.24, 0.46, 0.74), 0.55, 0.3)
    backdrop = bpy.data.materials.new("Quiet gradient")
    backdrop.use_nodes = True
    nodes, links = backdrop.node_tree.nodes, backdrop.node_tree.links
    nodes.clear()
    geometry = nodes.new("ShaderNodeNewGeometry")
    distance = nodes.new("ShaderNodeVectorMath")
    distance.operation = "DISTANCE"
    distance.inputs[1].default_value = (5.0, 2.0, -4.0)
    links.new(geometry.outputs["Position"], distance.inputs[0])
    radius = nodes.new("ShaderNodeMath")
    radius.operation = "DIVIDE"
    radius.inputs[1].default_value = 18.0
    links.new(distance.outputs["Value"], radius.inputs[0])
    ramp = nodes.new("ShaderNodeValToRGB")
    ramp.color_ramp.interpolation = "EASE"
    ramp.color_ramp.elements[0].color = (0.022, 0.052, 0.11, 1.0) if dark else (1.5, 1.7, 2.0, 1.0)
    ramp.color_ramp.elements[1].color = (0.002, 0.006, 0.016, 1.0) if dark else (0.9, 1.15, 1.5, 1.0)
    links.new(radius.outputs[0], ramp.inputs[0])
    emission = nodes.new("ShaderNodeEmission")
    links.new(ramp.outputs[0], emission.inputs["Color"])
    output = nodes.new("ShaderNodeOutputMaterial")
    links.new(emission.outputs[0], output.inputs["Surface"])

    fold = ribbon("Satin fold", -0.6, 0.7, pearl)
    fold.data.materials.append(blue)
    fold.modifiers["Satin edge"].material_offset = 1
    bpy.ops.mesh.primitive_plane_add(size=200, location=(0.0, 0.0, -4.0))
    bpy.context.object.data.materials.append(backdrop)

    area("Broad softbox", (-4.0, 3.0, 12.0), (0.81, 0.93, 1.0), 2400, 8.0)
    area("Upper rim", (7.0, 6.0, 8.0), (0.52, 0.82, 1.0), 1900, 6.0)
    area("Blue reflection", (1.0, -7.0, 5.0), (0.25, 0.46, 1.0) if dark else (0.75, 0.87, 1.0), 1100, 7.0)

    camera = bpy.data.cameras.new("Wallpaper camera")
    camera.type = "ORTHO"
    camera.ortho_scale = 21.0
    obj = bpy.data.objects.new("Wallpaper camera", camera)
    bpy.context.collection.objects.link(obj)
    obj.location = (-1.5, -7.0, 22.0)
    target = Vector((-1.5, 0.0, 0.0))
    obj.rotation_euler = (target - obj.location).to_track_quat("-Z", "Y").to_euler()
    scene.camera = obj
    DESTINATION.mkdir(parents=True, exist_ok=True)
    scene.render.filepath = str(DESTINATION / f"ferese-wallpaper-{appearance}.png")
    bpy.ops.render.render(write_still=True)
    Path(scene.render.filepath).chmod(0o644)


for appearance in ("dark", "light"):
    render(appearance)
