# Third-Party Code Attribution

This document provides attribution for third-party code used in this project.

## Buzz - Agent Client Protocol Implementation

**Source**: https://github.com/block/buzz  
**License**: Apache License 2.0  
**Copyright**: Copyright 2026 Block, Inc.

### Files Derived from Buzz

The following files in `src-rust/src/acp/pool/` are derived from the Buzz project:

1. **buzz_acp.rs** - ACP client implementation
   - Original: `crates/buzz-acp/src/acp.rs`
   - Modified: Yes (removed Buzz-specific observer/usage dependencies, created stub implementations)
   
2. **buzz_pool.rs** - Agent pool management
   - Original: `crates/buzz-acp/src/pool.rs`
   - Modified: Yes (removed Buzz-specific relay/queue dependencies)
   
3. **buzz_config.rs** - Configuration system
   - Original: `crates/buzz-acp/src/config.rs`
   - Modified: Yes (removed Buzz-specific Nostr integration)
   
4. **buzz_queue.rs** - Task queue management
   - Original: `crates/buzz-acp/src/queue.rs`
   - Modified: Yes (removed Buzz-specific relay dependencies)

### Modifications Made

The following modifications were made to adapt Buzz code for Ergatai:

1. **Removed Buzz-specific dependencies**:
   - Nostr protocol integration
   - Buzz relay client
   - Buzz-specific observer implementation
   - Buzz-specific usage tracking

2. **Added stub implementations**:
   - `observer.rs` - Minimal observer stub for compatibility
   - `usage.rs` - Minimal usage tracking stub for compatibility

3. **Updated imports**:
   - Changed `crate::observer` to `crate::pool::observer`
   - Changed `crate::usage` to `crate::pool::usage`

4. **Preserved core functionality**:
   - ACP protocol implementation (JSON-RPC 2.0 over stdio)
   - Agent pool management
   - Task queue and scheduling
   - Timeout and error handling
   - Permission request handling

### License Compliance

In compliance with Apache License 2.0 Section 4:

- ✅ **Section 4(a)**: This NOTICE file is included with the distribution
- ✅ **Section 4(b)**: Modified files carry prominent notices (see headers in each file)
- ✅ **Section 4(c)**: All copyright and attribution notices are retained
- ✅ **Section 4(d)**: This NOTICE file contains attribution notices

### Original License

```
                                 Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/

   Copyright 2026 Block, Inc.

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.
```

### How to Obtain the Original Source

The original Buzz source code can be obtained from:
- GitHub: https://github.com/block/buzz
- License: https://github.com/block/buzz/blob/main/LICENSE

## Contact

For questions about this attribution, please contact the Ergatai maintainers.
