#!/bin/sh
# Development-only synthetic camera: publish a known test image (or the
# GStreamer test pattern) as a PipeWire Video/Source node with the Camera
# role, the way a real camera appears to the XDG Camera portal.
# Usage: camera_source.sh <name> [image.png] [format]
# Requires gstreamer1.0-pipewire and gstreamer1.0-plugins-{base,good}. Never
# part of the application or its packaging.
name=${1:-agent-test-camera}
image=${2:-}
format=${3:-YUY2}
caps="video/x-raw,format=$format,width=640,height=480,framerate=15/1"
props="props,media.class=Video/Source,media.role=Camera,node.name=$name,node.description=$name"
if [ -n "$image" ]; then
  exec gst-launch-1.0 -q multifilesrc location="$image" loop=true caps=image/png,framerate=15/1 \
    ! pngdec ! videoconvert ! videoscale ! videorate ! "$caps" \
    ! pipewiresink mode=provide stream-properties="$props" client-name="$name"
fi
exec gst-launch-1.0 -q videotestsrc is-live=true pattern=smpte ! "$caps" \
  ! pipewiresink mode=provide stream-properties="$props" client-name="$name"
