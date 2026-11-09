#pragma once
#include <string>

// ABI плагіна 02.06.00.50 з Bambu Studio v02.06.00.51.
namespace bambu_0206 {
struct PrintParams {

    std::string dev_id;
    std::string task_name;
    std::string project_name;
    std::string preset_name;
    std::string filename;
    std::string config_filename;
    int plate_index;
    std::string ftp_folder;
    std::string ftp_file;
    std::string ftp_file_md5;
    std::string nozzle_mapping;
    std::string ams_mapping;
    std::string ams_mapping2;
    std::string ams_mapping_info;
    std::string nozzles_info;
    std::string connection_type;
    std::string comments;
    int origin_profile_id = 0;
    int stl_design_id = 0;
    std::string origin_model_id;
    std::string print_type;
    std::string dst_file;
    std::string dev_name;

    std::string dev_ip;
    bool use_ssl_for_ftp;
    bool use_ssl_for_mqtt;
    std::string username;
    std::string password;

    bool task_bed_leveling;
    bool task_flow_cali;
    bool task_vibration_cali;
    bool task_layer_inspect;
    bool task_record_timelapse;
    bool task_timelapse_use_internal;
    bool task_use_ams;
    std::string task_bed_type;
    std::string extra_options;
    int auto_bed_leveling{0};
    int auto_flow_cali{0};
    int auto_offset_cali{0};
    int extruder_cali_manual_mode{-1};
    bool task_ext_change_assist;
    bool try_emmc_print;
};
} // namespace bambu_0206

// ABI серії 02.08.04 (поточне покоління Orca, `NetworkAbi::Current`): той самий порядок
// полів, що в 02.06, плюс три рядки в кінці. Звірено з Orca d28c71c4 bambu_networking.hpp
// і Bambu Studio 79df7ae3 (BAMBU_NETWORK_AGENT_VERSION 02.08.04.57).
namespace bambu_0208 {
struct PrintParams {

    std::string dev_id;
    std::string task_name;
    std::string project_name;
    std::string preset_name;
    std::string filename;
    std::string config_filename;
    int plate_index;
    std::string ftp_folder;
    std::string ftp_file;
    std::string ftp_file_md5;
    std::string nozzle_mapping;
    std::string ams_mapping;
    std::string ams_mapping2;
    std::string ams_mapping_info;
    std::string nozzles_info;
    std::string connection_type;
    std::string comments;
    int origin_profile_id = 0;
    int stl_design_id = 0;
    std::string origin_model_id;
    std::string print_type;
    std::string dst_file;
    std::string dev_name;

    std::string dev_ip;
    bool use_ssl_for_ftp;
    bool use_ssl_for_mqtt;
    std::string username;
    std::string password;

    bool task_bed_leveling;
    bool task_flow_cali;
    bool task_vibration_cali;
    bool task_layer_inspect;
    bool task_record_timelapse;
    bool task_timelapse_use_internal;
    bool task_use_ams;
    std::string task_bed_type;
    std::string extra_options;
    int auto_bed_leveling{0};
    int auto_flow_cali{0};
    int auto_offset_cali{0};
    int extruder_cali_manual_mode{-1};
    bool task_ext_change_assist;
    bool try_emmc_print;
    std::string svc_context;
    std::string slicer_uid;
    std::string queue_plate_id;
};
} // namespace bambu_0208
