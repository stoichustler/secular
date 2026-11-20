// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <string>

#include "absl/container/flat_hash_set.h"

namespace google {
namespace protobuf {
class FileDescriptor;
}  // namespace protobuf
}  // namespace google

namespace upb {
class Arena;
class DefPool;

namespace generator {

class DefPoolPair;

// This function is called by src/google/protobuf/compiler/rust/upb_helpers.cc
// even though the protoc library does not use upb_generator. Trying to depend
// or compiler //third_party/protobuf/upb_generator fails due to the lack of
// auto-generated headers and pulls in abseil-cpp link-time dependencies.
//
// It is just simpler to make an empty version here to get the protoc compiler
// compiled properly in GN.
void MiniTableMessageVarName(std::string_view full_name) {}

void PopulateDefPool(const google::protobuf::FileDescriptor* file, upb::Arena* arena, DefPool* pool,
                     absl::flat_hash_set<std::string>* files_seen) {}

void PopulateDefPool(const google::protobuf::FileDescriptor* file, upb::Arena* arena,
                     DefPoolPair* pools, absl::flat_hash_set<std::string>* files_seen) {}

}  // namespace generator
}  // namespace upb
