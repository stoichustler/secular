#
# Copyright (c) 2018, Arm Limited and Contributors. All rights reserved.
#
# SPDX-License-Identifier: BSD-3-Clause
#

ZLIB_PATH	:=	lib/zlib

# Imported from zlib 1.2.11 (do not modify them)
LIBZLIB_SRCS	:=	$(addprefix $(ZLIB_PATH)/,	\
					adler32.c	\
					crc32.c		\
					inffast.c	\
					inflate.c	\
					inftrees.c	\
					zutil.c)

# Implemented for TF
LIBZLIB_SRCS	+=	$(addprefix $(ZLIB_PATH)/,	\
					tf_gunzip.c)

INCLUDES	+=	-Iinclude/lib/zlib

LIBZLIB_CFLAGS	:=	-DZ_SOLO -DDEF_WBITS=31
$(eval $(call MAKE_LIB,zlib))
