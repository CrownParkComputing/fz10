# CMake generated Testfile for 
# Source directory: /home/jon/retro3do-ios
# Build directory: /home/jon/retro3do-ios/build-linux
# 
# This file includes the relevant testing commands required for 
# testing this directory and lists subdirectories to be tested as well.
add_test("retro3do_tests" "/home/jon/retro3do-ios/build-linux/retro3do_tests")
set_tests_properties("retro3do_tests" PROPERTIES  _BACKTRACE_TRIPLES "/home/jon/retro3do-ios/CMakeLists.txt;407;add_test;/home/jon/retro3do-ios/CMakeLists.txt;0;")
add_test("retro3do_touch_tests" "/home/jon/retro3do-ios/build-linux/retro3do_touch_tests")
set_tests_properties("retro3do_touch_tests" PROPERTIES  _BACKTRACE_TRIPLES "/home/jon/retro3do-ios/CMakeLists.txt;413;add_test;/home/jon/retro3do-ios/CMakeLists.txt;0;")
add_test("retro3do_storage_tests" "/home/jon/retro3do-ios/build-linux/retro3do_storage_tests")
set_tests_properties("retro3do_storage_tests" PROPERTIES  _BACKTRACE_TRIPLES "/home/jon/retro3do-ios/CMakeLists.txt;422;add_test;/home/jon/retro3do-ios/CMakeLists.txt;0;")
subdirs("third_party/libchdr")
