#if defined(GLES2_RENDERER)
precision mediump float;
#define FRAG_COLOR gl_FragColor
#define TEXTURE texture2D

varying mediump vec2 texCoord;
#else
#define TEXTURE texture

in vec2 texCoord;

out vec4 FragColor;
#define FRAG_COLOR FragColor
#endif

uniform sampler2D image;

void main() {
    vec4 color = TEXTURE(image, texCoord);

    // Output premultiplied alpha.
    FRAG_COLOR = vec4(color.rgb * color.a, color.a);
}
