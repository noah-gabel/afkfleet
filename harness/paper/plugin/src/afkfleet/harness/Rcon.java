package afkfleet.harness;

import java.io.DataInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Properties;

/**
 * The tests' RCON client, run inside the harness's container as {@code rcon <command>}, as the
 * vanilla test server's {@code rcon-cli}: it reads the port and the password the container's start
 * wrote into {@code server.properties}, sends the command to 127.0.0.1 and prints the answer. The
 * RCON port is never published.
 */
public final class Rcon {
    private static final int AUTH = 3;
    private static final int COMMAND = 2;
    /** How long a connect or a read may take. */
    private static final int TIMEOUT_MS = 10_000;

    private Rcon() {}

    public static void main(String[] args) throws IOException {
        Properties properties = new Properties();
        try (InputStream in = Files.newInputStream(Path.of("/paper/server.properties"))) {
            properties.load(in);
        }
        int port = Integer.parseInt(properties.getProperty("rcon.port", "25575"));
        String password = properties.getProperty("rcon.password", "");
        try (Socket socket = new Socket()) {
            socket.connect(new InetSocketAddress("127.0.0.1", port), TIMEOUT_MS);
            socket.setSoTimeout(TIMEOUT_MS);
            OutputStream out = socket.getOutputStream();
            DataInputStream in = new DataInputStream(socket.getInputStream());
            write(out, 1, AUTH, password);
            if (read(in).id() != 1) {
                System.err.println("RCON refused the password");
                System.exit(1);
            }
            write(out, 2, COMMAND, String.join(" ", args));
            System.out.println(read(in).body());
        }
    }

    private record Packet(int id, String body) {}

    private static void write(OutputStream out, int id, int type, String body) throws IOException {
        byte[] text = body.getBytes(StandardCharsets.UTF_8);
        ByteBuffer packet = ByteBuffer.allocate(14 + text.length).order(ByteOrder.LITTLE_ENDIAN);
        packet.putInt(10 + text.length).putInt(id).putInt(type).put(text).put((byte) 0).put((byte) 0);
        out.write(packet.array());
        out.flush();
    }

    private static Packet read(DataInputStream in) throws IOException {
        byte[] head = new byte[4];
        in.readFully(head);
        int length = ByteBuffer.wrap(head).order(ByteOrder.LITTLE_ENDIAN).getInt();
        if (length < 10 || length > 1 << 20) {
            throw new IOException("an RCON packet of " + length + " bytes");
        }
        byte[] rest = new byte[length];
        in.readFully(rest);
        ByteBuffer packet = ByteBuffer.wrap(rest).order(ByteOrder.LITTLE_ENDIAN);
        int id = packet.getInt();
        packet.getInt();
        return new Packet(id, new String(rest, 8, length - 10, StandardCharsets.UTF_8));
    }
}
