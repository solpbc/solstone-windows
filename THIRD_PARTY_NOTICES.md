# third-party notices

## Microsoft Visual C++ runtime and Universal CRT components

the solstone app for windows embeds Microsoft Visual C++ runtime and Universal
CRT code in its executable.

Copyright (c) Microsoft Corporation. All rights reserved.

these Microsoft components are licensed separately by Microsoft and are not
covered by solstone's AGPL-3.0-only license.

the Microsoft Visual C++ runtime is provided with Microsoft Visual Studio
Enterprise 2022 under the Microsoft Visual Studio Enterprise 2022 and
Microsoft Visual Studio Professional 2022 License Terms. the Universal CRT is
provided with the Microsoft Windows Software Development Kit (SDK) for Windows
10 under its license terms and REDIST list. the terms reviewed for this build
are published at:

- https://visualstudio.microsoft.com/wp-content/uploads/2021/11/Visual-Studio-2022-Enterprise-Professional-License-EN.docx
- https://learn.microsoft.com/en-us/legal/windows-sdk/license
- https://learn.microsoft.com/en-us/legal/windows-sdk/redist

the following conditions from the "Scope of License" and "Distribution
Requirements" sections of those terms apply only to the Microsoft components:

- redistribute them only as embedded in this program, unmodified;
- except where applicable law permits otherwise, do not reverse engineer,
  decompile or disassemble them; and
- do not remove or alter any Microsoft notice they carry.

Microsoft provides these components as is and gives no warranty for them.
Microsoft does not sponsor or endorse solstone.

these terms apply only to the Microsoft components. they do not restrict the
AGPL-licensed solstone code.

## Microsoft Edge WebView2 loader

the solstone app for windows embeds the Microsoft Edge WebView2 loader in its
executable. the loader is linked statically from WebView2LoaderStatic.lib in
Microsoft's WebView2 SDK, NuGet package Microsoft.Web.WebView2 version
1.0.3650.58.

Microsoft licenses the loader under the BSD 3-Clause license reproduced below.
that license, not solstone's AGPL-3.0-only license, governs this component.
Microsoft does not sponsor or endorse solstone.

Copyright (C) Microsoft Corporation. All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are
met:

   * Redistributions of source code must retain the above copyright
notice, this list of conditions and the following disclaimer.
   * Redistributions in binary form must reproduce the above
copyright notice, this list of conditions and the following disclaimer
in the documentation and/or other materials provided with the
distribution.
   * The name of Microsoft Corporation, or the names of its contributors
may not be used to endorse or promote products derived from this
software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
