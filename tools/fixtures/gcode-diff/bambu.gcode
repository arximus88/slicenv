; HEADER_BLOCK_START
; BambuStudio 02.07.01.62
; model printing time: 1m 0s; total estimated time: 1m 30s
; total layer number: 2
; total filament length [mm] : 12.34
; total filament volume [cm^3] : 29.68
; total filament weight [g] : 0.04
; filament_density: 1.26
; filament_diameter: 1.75
; max_z_height: 0.40
; HEADER_BLOCK_END

; THUMBNAIL_BLOCK_START
; thumbnail begin 4x4 32
; iVBORw0KGgoAAAANSUhEUgAAAAQAAAAEAQMAAACTPww9AAAAA1BMVEX/AAAZ4gk3AAAA
; thumbnail end
; THUMBNAIL_BLOCK_END

; CONFIG_BLOCK_START
; layer_height = 0.2
; nozzle_temperature = 220
; post_process = /home/someone/scripts/fix.sh
; sparse_infill_density = 15%
; CONFIG_BLOCK_END

; EXECUTABLE_BLOCK_START
M73 P0 R1
M104 S220
G28
G1 Z5 F3000
; CHANGE_LAYER
; Z_HEIGHT: 0.2
; LAYER_HEIGHT: 0.2
G1 Z.2 F600
G1 X10 Y10 F12000
G1 X20 Y10 E.5 F1500
G1 X20 Y20 E.5
M73 P50 R0
; CHANGE_LAYER
; Z_HEIGHT: 0.4
; LAYER_HEIGHT: 0.2
G1 Z.4 F600
G1 X10 Y20 E.5 F1500
G1 X10 Y10 E.5
M104 S0
M73 P100 R0
; EXECUTABLE_BLOCK_END
