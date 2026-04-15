(function () {
    const VS = `#version 300 es
in vec2 position;
out vec2 v_uv;
void main() {
    v_uv = vec2((position.x + 1.0) * 0.5, 1.0 - (position.y + 1.0) * 0.5);
    gl_Position = vec4(position, 0.0, 1.0);
}`;

    const FS_NV12 = `#version 300 es
precision mediump float;
in vec2 v_uv;
out vec4 frag_color;
uniform sampler2D y_tex;
uniform sampler2D uv_tex;
uniform vec2 u_crop;
void main() {
    vec2 uv = v_uv * u_crop;
    float y = texture(y_tex, uv).r;
    vec2 cbcr = texture(uv_tex, uv).rg;
    float cb = cbcr.r - 0.5;
    float cr = cbcr.g - 0.5;
    float c = y - 0.0625;
    float r = clamp(1.164 * c + 1.596 * cr, 0.0, 1.0);
    float g = clamp(1.164 * c - 0.391 * cb - 0.813 * cr, 0.0, 1.0);
    float b = clamp(1.164 * c + 2.018 * cb, 0.0, 1.0);
    frag_color = vec4(r, g, b, 1.0);
}`;

    const FS_PACKED = `#version 300 es
precision mediump float;
in vec2 v_uv;
out vec4 frag_color;
uniform sampler2D rgba_tex;
uniform vec2 u_crop;
uniform float u_swap_rb;
void main() {
    vec4 c = texture(rgba_tex, v_uv * u_crop);
    vec3 rgb = mix(c.rgb, c.bgr, u_swap_rb);
    frag_color = vec4(rgb, 1.0);
}`;

    function setupCanvas(canvas) {
        if (canvas._chimerasInit) return;
        canvas._chimerasInit = true;
        const url = canvas.dataset.previewUrl;
        if (!url) return;
        const gl = canvas.getContext("webgl2", { alpha: false, antialias: false });
        if (!gl) return;

        function compile(src, type) {
            const s = gl.createShader(type);
            gl.shaderSource(s, src);
            gl.compileShader(s);
            if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
                gl.deleteShader(s);
                return null;
            }
            return s;
        }
        function link(vs, fs) {
            const p = gl.createProgram();
            gl.attachShader(p, vs);
            gl.attachShader(p, fs);
            gl.linkProgram(p);
            if (!gl.getProgramParameter(p, gl.LINK_STATUS)) {
                gl.deleteProgram(p);
                return null;
            }
            return p;
        }
        const vs = compile(VS, gl.VERTEX_SHADER);
        const progNv12 = link(vs, compile(FS_NV12, gl.FRAGMENT_SHADER));
        const progPacked = link(vs, compile(FS_PACKED, gl.FRAGMENT_SHADER));

        const quad = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, quad);
        gl.bufferData(
            gl.ARRAY_BUFFER,
            new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]),
            gl.STATIC_DRAW,
        );
        function bindQuad(program) {
            const loc = gl.getAttribLocation(program, "position");
            gl.bindBuffer(gl.ARRAY_BUFFER, quad);
            gl.enableVertexAttribArray(loc);
            gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
        }
        function makeTexture() {
            const t = gl.createTexture();
            gl.bindTexture(gl.TEXTURE_2D, t);
            gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
            gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
            gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
            gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
            return t;
        }
        const yTex = makeTexture();
        const uvTex = makeTexture();
        const packedTex = makeTexture();

        let fetching = false;
        let lastCounter = 0;
        let currentFormat = 0;
        let currentWidth = 0;
        let currentHeight = 0;
        let currentStride = 0;
        let yTexDims = { w: 0, h: 0 };
        let uvTexDims = { w: 0, h: 0 };
        let packedTexDims = { w: 0, h: 0 };

        async function fetchAndUpload() {
            if (fetching) return;
            fetching = true;
            try {
                const response = await fetch(url, { cache: "no-store" });
                if (!response.ok) return;
                const buffer = await response.arrayBuffer();
                if (buffer.byteLength < 24) return;
                const view = new DataView(buffer);
                if (
                    view.getUint8(0) !== 0x43 ||
                    view.getUint8(1) !== 0x48 ||
                    view.getUint8(2) !== 0x49 ||
                    view.getUint8(3) !== 0x4d
                ) {
                    return;
                }
                const format = view.getUint8(5);
                const width = view.getUint32(8, true);
                const height = view.getUint32(12, true);
                const stride = view.getUint32(16, true);
                const counter = view.getUint32(20, true);
                if (counter === lastCounter && format === currentFormat) return;
                lastCounter = counter;
                currentFormat = format;
                currentWidth = width;
                currentHeight = height;
                currentStride = stride;
                if (format === 0 || width === 0 || height === 0) return;
                const payload = new Uint8Array(buffer, 24);
                gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
                if (format === 1) {
                    const ySize = stride * height;
                    const uvSize = stride * (height >> 1);
                    if (payload.byteLength < ySize + uvSize) return;
                    const yBytes = payload.subarray(0, ySize);
                    const uvBytes = payload.subarray(ySize, ySize + uvSize);
                    gl.activeTexture(gl.TEXTURE0);
                    gl.bindTexture(gl.TEXTURE_2D, yTex);
                    if (yTexDims.w !== stride || yTexDims.h !== height) {
                        gl.texImage2D(
                            gl.TEXTURE_2D,
                            0,
                            gl.R8,
                            stride,
                            height,
                            0,
                            gl.RED,
                            gl.UNSIGNED_BYTE,
                            yBytes,
                        );
                        yTexDims = { w: stride, h: height };
                    } else {
                        gl.texSubImage2D(
                            gl.TEXTURE_2D,
                            0,
                            0,
                            0,
                            stride,
                            height,
                            gl.RED,
                            gl.UNSIGNED_BYTE,
                            yBytes,
                        );
                    }
                    const uvW = stride >> 1;
                    const uvH = height >> 1;
                    gl.activeTexture(gl.TEXTURE1);
                    gl.bindTexture(gl.TEXTURE_2D, uvTex);
                    if (uvTexDims.w !== uvW || uvTexDims.h !== uvH) {
                        gl.texImage2D(
                            gl.TEXTURE_2D,
                            0,
                            gl.RG8,
                            uvW,
                            uvH,
                            0,
                            gl.RG,
                            gl.UNSIGNED_BYTE,
                            uvBytes,
                        );
                        uvTexDims = { w: uvW, h: uvH };
                    } else {
                        gl.texSubImage2D(
                            gl.TEXTURE_2D,
                            0,
                            0,
                            0,
                            uvW,
                            uvH,
                            gl.RG,
                            gl.UNSIGNED_BYTE,
                            uvBytes,
                        );
                    }
                } else if (format === 2 || format === 3) {
                    const rowPixels = stride >> 2;
                    if (payload.byteLength < stride * height) return;
                    const slice = payload.subarray(0, stride * height);
                    gl.activeTexture(gl.TEXTURE0);
                    gl.bindTexture(gl.TEXTURE_2D, packedTex);
                    if (packedTexDims.w !== rowPixels || packedTexDims.h !== height) {
                        gl.texImage2D(
                            gl.TEXTURE_2D,
                            0,
                            gl.RGBA8,
                            rowPixels,
                            height,
                            0,
                            gl.RGBA,
                            gl.UNSIGNED_BYTE,
                            slice,
                        );
                        packedTexDims = { w: rowPixels, h: height };
                    } else {
                        gl.texSubImage2D(
                            gl.TEXTURE_2D,
                            0,
                            0,
                            0,
                            rowPixels,
                            height,
                            gl.RGBA,
                            gl.UNSIGNED_BYTE,
                            slice,
                        );
                    }
                }
            } catch (_) {
            } finally {
                fetching = false;
            }
        }

        function resize() {
            const parent = canvas.parentElement;
            if (!parent) return;
            const rect = parent.getBoundingClientRect();
            const ratio = window.devicePixelRatio || 1;
            const w = Math.max(1, Math.floor(rect.width * ratio));
            const h = Math.max(1, Math.floor(rect.height * ratio));
            if (canvas.width !== w || canvas.height !== h) {
                canvas.width = w;
                canvas.height = h;
            }
        }

        function render() {
            resize();
            gl.viewport(0, 0, canvas.width, canvas.height);
            gl.clearColor(0, 0, 0, 1);
            gl.clear(gl.COLOR_BUFFER_BIT);
            if (currentFormat === 1 && currentWidth > 0 && currentHeight > 0) {
                const cropX = currentStride > 0 ? currentWidth / currentStride : 1;
                gl.useProgram(progNv12);
                bindQuad(progNv12);
                gl.uniform1i(gl.getUniformLocation(progNv12, "y_tex"), 0);
                gl.uniform1i(gl.getUniformLocation(progNv12, "uv_tex"), 1);
                gl.uniform2f(gl.getUniformLocation(progNv12, "u_crop"), cropX, 1.0);
                gl.activeTexture(gl.TEXTURE0);
                gl.bindTexture(gl.TEXTURE_2D, yTex);
                gl.activeTexture(gl.TEXTURE1);
                gl.bindTexture(gl.TEXTURE_2D, uvTex);
                gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
            } else if (
                (currentFormat === 2 || currentFormat === 3)
                && currentWidth > 0
                && currentHeight > 0
            ) {
                const rowPixels = currentStride >> 2;
                const cropX = rowPixels > 0 ? currentWidth / rowPixels : 1;
                gl.useProgram(progPacked);
                bindQuad(progPacked);
                gl.uniform1i(gl.getUniformLocation(progPacked, "rgba_tex"), 0);
                gl.uniform2f(gl.getUniformLocation(progPacked, "u_crop"), cropX, 1.0);
                gl.uniform1f(
                    gl.getUniformLocation(progPacked, "u_swap_rb"),
                    currentFormat === 2 ? 1.0 : 0.0,
                );
                gl.activeTexture(gl.TEXTURE0);
                gl.bindTexture(gl.TEXTURE_2D, packedTex);
                gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
            }
        }

        let lastFetchStart = 0;
        function loop() {
            if (!canvas.isConnected) return;
            const now = performance.now();
            if (!fetching && now - lastFetchStart > 28) {
                lastFetchStart = now;
                fetchAndUpload();
            }
            render();
            requestAnimationFrame(loop);
        }
        requestAnimationFrame(loop);
    }

    function scan() {
        document
            .querySelectorAll("canvas[data-stream-id]")
            .forEach(setupCanvas);
    }

    const observer = new MutationObserver(() => scan());
    observer.observe(document.body, { childList: true, subtree: true });
    scan();
})();
