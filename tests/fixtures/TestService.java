import java.net.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.concurrent.CountDownLatch;

/** Isolated integration-test fixture: never uses business ports or directories. */
public class TestService {
    public static void main(String[] args) throws Exception {
        ServerSocket tcp4 = new ServerSocket(0, 8, InetAddress.getByName("127.0.0.1"));
        ServerSocket tcp6 = new ServerSocket(0, 8, InetAddress.getByName("::1"));
        DatagramSocket udp4 = new DatagramSocket(new InetSocketAddress("127.0.0.1", 0));
        DatagramSocket udp6 = new DatagramSocket(new InetSocketAddress("::1", 0));
        String ready = "TCP:" + tcp4.getLocalPort() + "\nTCP:" + tcp6.getLocalPort()
            + "\nUDP:" + udp4.getLocalPort() + "\nUDP:" + udp6.getLocalPort();
        Files.write(Paths.get("ready.txt"), ready.getBytes(StandardCharsets.UTF_8));
        Files.createDirectories(Paths.get("logs"));
        Files.write(Paths.get("logs", "witlink-cis-fixture.log"), "fixture ready\n".getBytes(StandardCharsets.UTF_8));
        System.out.println("TestService ready");
        System.err.println("TestService stderr ready");
        new CountDownLatch(1).await();
    }
}
