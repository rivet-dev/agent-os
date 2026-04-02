#ifndef REGISTRY_NATIVE_C_INCLUDE_SYS_IOCTL_H
#define REGISTRY_NATIVE_C_INCLUDE_SYS_IOCTL_H

#include_next <sys/ioctl.h>

#ifndef __DEFINED_struct_winsize
struct winsize {
	unsigned short ws_row;
	unsigned short ws_col;
	unsigned short ws_xpixel;
	unsigned short ws_ypixel;
};
#define __DEFINED_struct_winsize
#endif

#ifndef TIOCGWINSZ
#define TIOCGWINSZ 0x5413
#endif

#endif
