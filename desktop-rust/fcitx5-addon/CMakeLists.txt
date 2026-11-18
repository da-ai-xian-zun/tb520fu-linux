cmake_minimum_required(VERSION 3.16)
project(fcitx5-tb520fu-osk VERSION 0.1 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Debian 13 Trixie ships fcitx5 5.1.12; accept any 5.1.x+
find_package(Fcitx5Core 5.1.0 REQUIRED)
find_package(Fcitx5ModuleDBus REQUIRED)
include("${FCITX_INSTALL_CMAKECONFIG_DIR}/Fcitx5Utils/Fcitx5CompilerSettings.cmake")

add_fcitx5_addon(y700osk y700osk.cpp)
target_link_libraries(y700osk Fcitx5::Core Fcitx5::Module::DBus)

install(TARGETS y700osk DESTINATION "${FCITX_INSTALL_ADDONDIR}")
install(FILES y700osk.conf DESTINATION "${FCITX_INSTALL_PKGDATADIR}/addon")
