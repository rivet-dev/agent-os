if(NOT TARGET Threads::Threads)
  add_library(Threads::Threads INTERFACE IMPORTED)
  set_target_properties(
    Threads::Threads
    PROPERTIES
      INTERFACE_COMPILE_DEFINITIONS "_WASI_EMULATED_PTHREAD"
      INTERFACE_LINK_LIBRARIES "wasi-emulated-pthread"
  )
endif()

set(CMAKE_THREAD_LIBS_INIT "wasi-emulated-pthread" CACHE STRING "" FORCE)
set(CMAKE_USE_PTHREADS_INIT 0 CACHE BOOL "" FORCE)
set(CMAKE_USE_WIN32_THREADS_INIT 0 CACHE BOOL "" FORCE)
set(Threads_FOUND TRUE)
set(THREADS_FOUND TRUE)
